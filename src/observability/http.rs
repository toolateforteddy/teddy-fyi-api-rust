//! Per-request logging and metrics.
//!
//! Emits exactly one structured line per completed request, tagged
//! `event="http_request"`. That line is load-bearing twice over: it is the
//! debugging record, and — because product analytics here is built on Cloud
//! Logging log-based metrics rather than an events table — it is also the
//! analytics schema. Log-based metrics cannot be backfilled, so a field left out
//! today is a field with no history tomorrow. Treat the set below as append-only.

use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};
use sha2::{Digest, Sha256};
use std::time::Instant;

/// Placeholder for a field that does not apply to this request — an
/// unauthenticated route has no user, a rejected one may have no client. Logged
/// explicitly rather than omitted so every `http_request` line has the same
/// shape and log-based metrics never see a missing key.
const ABSENT: &str = "-";

/// The user identifier attached to a response for logging.
///
/// Holds a **salted hash**, never the raw user id. Cloud Logging is outside the
/// reach of both `DELETE /api/user/data` and `jobs::reap_stale_users`, so a raw
/// id in the logs would be a copy of user-identifying data that neither erasure
/// path can reach — awkward next to the published retention commitment. Hashing
/// keeps per-user *counting* (distinct-user analytics, per-user debugging within
/// one retention window) while making the un-erasable copy non-identifying.
#[derive(Clone, Debug)]
pub struct LoggedUser(pub String);

/// Salted, truncated SHA-256 of a user id.
///
/// The salt is `LOG_HASH_SALT` when set, falling back to `JWT_SECRET` — which is
/// already required at boot, already secret, and already rotated with the
/// deployment. Domain-separated with a prefix so the digest can never coincide
/// with anything else derived from that secret. Truncated to 16 hex characters:
/// ample to keep collisions negligible at this scale, and less material to leak.
pub fn hash_user_id(user_id: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"teddy-fyi/log-user-id/v1:");
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(user_id.as_bytes());
    let digest = hasher.finalize();
    digest[..8].iter().map(|b| format!("{:02x}", b)).collect()
}

/// Reads the salt once per call site; see [`hash_user_id`] for the fallback.
pub fn log_hash_salt(jwt_secret: &str) -> String {
    std::env::var("LOG_HASH_SALT").unwrap_or_else(|_| jwt_secret.to_string())
}

/// Middleware emitting the `http_request` log line and the RED metrics.
///
/// Applied with `Router::layer`, so routing has already happened and
/// [`MatchedPath`] is available. Using the matched path (`/api/devices/:id`)
/// rather than the raw URI is not cosmetic: the raw URI would put an unbounded
/// number of ids into a Prometheus label and blow up the time series.
pub async fn track_request(req: Request, next: Next) -> Response {
    let start = Instant::now();

    let method = req.method().clone();
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_string())
        // A request that matched no route. Constant, not the URI — an unrouted
        // path is exactly where hostile or accidental cardinality comes from.
        .unwrap_or_else(|| "unmatched".to_string());

    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or(ABSENT)
        .to_string();

    let client_uuid = req
        .headers()
        .get("x-client-uuid")
        .and_then(|value| value.to_str().ok())
        .unwrap_or(ABSENT)
        .to_string();

    let response = next.run(req).await;

    // Kubernetes probes hit `/healthz/*` every few seconds forever. Logging and
    // counting them would drown the request log, inflate every log-based metric
    // with traffic no user generated, and buy nothing: probe results are already
    // visible through the Deployment, and a failing readiness check logs its own
    // warning with the reason attached.
    if route.starts_with("/healthz") {
        return response;
    }

    let latency = start.elapsed();
    let status = response.status().as_u16();

    // `require_auth` attaches this on the way out; requests it rejected, and
    // unauthenticated routes, have none.
    let user_hash = response
        .extensions()
        .get::<LoggedUser>()
        .map(|user| user.0.as_str())
        .unwrap_or(ABSENT);

    metrics::counter!(
        "http_requests_total",
        "method" => method.as_str().to_string(),
        "route" => route.clone(),
        "status" => status.to_string(),
    )
    .increment(1);
    metrics::histogram!(
        "http_request_duration_seconds",
        "method" => method.as_str().to_string(),
        "route" => route.clone(),
    )
    .record(latency.as_secs_f64());

    tracing::info!(
        event = "http_request",
        method = %method,
        route = %route,
        status = status,
        latency_ms = latency.as_millis() as u64,
        request_id = %request_id,
        client_uuid = %client_uuid,
        user_hash = %user_hash,
        "request completed"
    );

    response
}

/// Records one completed `POST /api/sync`.
///
/// Separate from the `http_request` line because the interesting analytics for a
/// sync backend is not "how many requests" but "how much moved, and in which
/// scope" — and `scope` lives in the request body, which middleware cannot see
/// without buffering it. `sync_success_count` derived from this event is also
/// what the "no successful sync in N minutes" alert watches; for a sync service
/// that is the one alert that catches broken-but-still-returning-200.
pub fn record_sync_completed(scope: &str, uploaded: usize, downloaded: usize) {
    metrics::counter!("sync_completed_total", "scope" => scope.to_string()).increment(1);
    metrics::counter!("sync_entities_uploaded_total", "scope" => scope.to_string())
        .increment(uploaded as u64);
    metrics::counter!("sync_entities_downloaded_total", "scope" => scope.to_string())
        .increment(downloaded as u64);

    tracing::info!(
        event = "sync_completed",
        scope = %scope,
        uploaded = uploaded,
        downloaded = downloaded,
        "sync completed"
    );
}

/// Salted, truncated SHA-256 of a rejected request body.
///
/// Same reasoning as [`hash_user_id`], applied to payload bytes: Cloud Logging is
/// outside the reach of both `DELETE /api/user/data` and `jobs::reap_stale_users`,
/// so a request body written there is an un-erasable copy of exactly the data the
/// sync endpoint carries — a child's drawing strokes and config values. The hash
/// keeps the one diagnostic property that mattered about logging the body at all:
/// identical malformed payloads correlate across requests, so a client stuck in a
/// retry loop is still recognisable as one bug rather than N.
///
/// Domain-separated with its own prefix so a body digest can never collide with a
/// user-id digest derived from the same salt, and truncated to the same 16 hex
/// characters for the same reasons.
pub fn hash_log_body(body: &[u8], salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"teddy-fyi/log-body/v1:");
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(body);
    let digest = hasher.finalize();
    digest[..8].iter().map(|b| format!("{:02x}", b)).collect()
}

/// The salt, for call sites with no `AppState` in reach.
///
/// [`log_hash_salt`] takes the secret from state because its callers have it.
/// An extractor runs before any handler and is generic over the state type, so it
/// reads the same two variables straight from the environment instead — `main`
/// already `expect`s `JWT_SECRET` at boot, so the fallback is present whenever the
/// process is. The empty-string last resort only happens in tests, where an
/// unsalted digest of a test fixture discloses nothing.
pub fn log_hash_salt_from_env() -> String {
    std::env::var("LOG_HASH_SALT")
        .or_else(|_| std::env::var("JWT_SECRET"))
        .unwrap_or_default()
}
