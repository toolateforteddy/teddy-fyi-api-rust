//! Prometheus metrics: the recorder, the scrape listener, and the handful of
//! counters that live outside the request path.
//!
//! The scrape endpoint deliberately binds its **own** listener (`METRICS_PORT`,
//! default 9090) rather than joining the main router. Google Managed Prometheus
//! scrapes the pod IP directly, so `/metrics` never has to be reachable through
//! the ingress and never needs an exemption carved out of `require_auth`.

use axum::{routing::get, Router};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder, PrometheusHandle};

/// Buckets for `http_request_duration_seconds`, in seconds. Weighted towards the
/// low end: a sync round trip that takes 10s is already a bad day, and the
/// interesting resolution is between 10ms and 1s.
const LATENCY_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

/// Installs the process-wide Prometheus recorder.
///
/// Returns `None` if a recorder is already installed, which is the normal case
/// under `cargo test`: the recorder is global, tests share a process, and the
/// second installation would otherwise panic. Metric calls with no recorder
/// installed are no-ops, so tests still exercise the instrumented code paths.
pub fn init_recorder() -> Option<PrometheusHandle> {
    let builder = PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full("http_request_duration_seconds".to_string()),
            LATENCY_BUCKETS,
        )
        .expect("latency buckets are non-empty")
        .set_buckets_for_metric(
            Matcher::Full("gemini_request_duration_seconds".to_string()),
            LATENCY_BUCKETS,
        )
        .expect("latency buckets are non-empty");

    match builder.install_recorder() {
        Ok(handle) => {
            register_baseline_metrics();
            Some(handle)
        }
        Err(err) => {
            tracing::warn!("Prometheus recorder not installed: {:?}", err);
            None
        }
    }
}

/// Emits the metrics that may legitimately never fire, so they exist at zero.
///
/// A Prometheus exporter only renders a series once something has recorded to it.
/// Without this, a healthy service reports *nothing* for `redis_degraded_total`,
/// which on a dashboard is "no data" — indistinguishable from a broken exporter
/// or an un-deployed build, and exactly the ambiguity monitoring is supposed to
/// remove. Describing them also gives the scrape output real HELP text.
fn register_baseline_metrics() {
    metrics::describe_counter!(
        "redis_degraded_total",
        "Redis operations that failed and were swallowed by a fallback path"
    );
    metrics::describe_gauge!(
        "sse_connections_active",
        "Currently-connected /api/sync/stream clients"
    );
    metrics::describe_counter!(
        "gemini_requests_total",
        "Calls to the Gemini API, by model and outcome"
    );
    metrics::describe_counter!(
        "gemini_calls_refused_total",
        "Gemini calls refused before dispatch by a spend budget or the kill switch"
    );
    metrics::describe_counter!("sync_completed_total", "Successful POST /api/sync requests");
    metrics::describe_gauge!(
        "db_connectivity_degraded",
        "1 when this replica infers Postgres is unreachable from recent request failures"
    );
    metrics::describe_counter!(
        "db_connectivity_failures_total",
        "Database errors indicating unreachability or pool exhaustion"
    );
    metrics::describe_counter!("http_requests_total", "HTTP requests, by route and status");
    metrics::describe_counter!(
        "sse_streams_refused_total",
        "/api/sync/stream connections refused by a concurrency cap, by reason"
    );
    metrics::describe_counter!(
        "http_requests_shed_total",
        "Requests refused with 503 because the concurrency ceiling was full"
    );
    metrics::describe_counter!(
        "http_handler_panics_total",
        "Handler panics caught by the panic guard and answered with 500"
    );

    metrics::gauge!("sse_connections_active").set(0.0);
    // Both of these are supposed to stay at zero forever, which is precisely why they
    // have to exist at zero: "no data" and "nothing has gone wrong" must not look alike.
    metrics::counter!("http_requests_shed_total").increment(0);
    metrics::counter!("http_handler_panics_total").increment(0);
    metrics::gauge!("db_connectivity_degraded").set(0.0);
    for class in ["unreachable", "saturated"] {
        metrics::counter!("db_connectivity_failures_total", "class" => class).increment(0);
    }
    // A cap that is never hit is the healthy state, and the series has to exist
    // at zero for "no data" not to be indistinguishable from "no abuse".
    for reason in ["per_user", "global"] {
        metrics::counter!("sse_streams_refused_total", "reason" => reason).increment(0);
    }
    // Same reasoning as the stream caps: a spend budget that is never hit is the
    // healthy state, so its series must exist at zero rather than be absent.
    for reason in crate::routes::ai::budget::REFUSAL_REASONS {
        metrics::counter!("gemini_calls_refused_total", "reason" => *reason).increment(0);
    }
    for site in REDIS_FALLBACK_SITES {
        metrics::counter!("redis_degraded_total", "site" => *site).increment(0);
    }
}

/// Every `site` label [`record_redis_degraded`] can emit.
///
/// Listed so each series starts at zero; keep in step with the call sites.
const REDIS_FALLBACK_SITES: &[&str] = &[
    "sync_status_connect",
    "sync_status_get",
    "sync_status_backfill_connect",
    "sync_status_backfill_set",
    "sync_cache_write_connect",
    "gemini_budget_connect",
    "gemini_budget_incr",
];

/// Serves `GET /metrics` on its own port until the process exits.
///
/// Failures here are logged and dropped rather than fatal: losing the scrape
/// endpoint should never take the API down with it.
pub async fn serve_metrics(handle: PrometheusHandle) {
    let port = std::env::var("METRICS_PORT").unwrap_or_else(|_| "9090".to_string());
    let addr = format!("0.0.0.0:{}", port);

    let app = Router::new().route(
        "/metrics",
        get(move || {
            let handle = handle.clone();
            async move { handle.render() }
        }),
    );

    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(err) => {
            tracing::error!("Failed to bind metrics listener on {}: {:?}", addr, err);
            return;
        }
    };

    tracing::info!("Metrics listening on {}", addr);
    if let Err(err) = axum::serve(listener, app).await {
        tracing::error!("Metrics server stopped: {:?}", err);
    }
}

/// Records a Redis operation that failed and was swallowed by a fallback path.
///
/// Every Redis call site in this service is written as `if let Ok(conn) = ...`,
/// so a Redis outage degrades silently: `/api/sync/status` just falls through to
/// the expensive database aggregate. This counter is what turns that deliberate
/// silence into something a dashboard can show and an alert can fire on.
///
/// `site` must be a `&'static str` so the label set stays bounded.
pub fn record_redis_degraded(site: &'static str) {
    metrics::counter!("redis_degraded_total", "site" => site).increment(1);
}

/// Records the outcome and duration of one Gemini call. The only spend-shaped
/// metric in the service.
pub fn record_gemini_call(model: &str, outcome: &'static str, duration_secs: f64) {
    metrics::counter!(
        "gemini_requests_total",
        "model" => model.to_string(),
        "outcome" => outcome,
    )
    .increment(1);
    metrics::histogram!("gemini_request_duration_seconds", "model" => model.to_string())
        .record(duration_secs);
}

/// Holds the `sse_connections_active` gauge up for as long as it is alive.
///
/// `/api/sync/stream` connections are long-lived, so the request metrics count
/// them once at open and never see them again — without this gauge the stream
/// endpoint has effectively no observability. Tied to `Drop` rather than to an
/// explicit close call because there is no such call: the stream ends when the
/// client vanishes, and only the destructor reliably observes that.
pub struct SseConnectionGuard;

impl SseConnectionGuard {
    pub fn open() -> Self {
        metrics::gauge!("sse_connections_active").increment(1.0);
        Self
    }
}

impl Drop for SseConnectionGuard {
    fn drop(&mut self) {
        metrics::gauge!("sse_connections_active").decrement(1.0);
    }
}
