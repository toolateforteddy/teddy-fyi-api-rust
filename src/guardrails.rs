//! Transport-level guardrails: the tower layers that bound what a single client, or a
//! single bad request, can cost this process.
//!
//! Everything here is about survivability rather than correctness. The handlers are
//! written as if the caller is a well-behaved Android client on a good network; these
//! layers are what stands between that assumption and a client that is neither. Five
//! separate failure modes, five sets of layers:
//!
//! * **No request deadline.** A stalled client held a handler — and the Postgres
//!   connection it borrowed from a pool of five — for as long as it cared to. A handful
//!   of stalled requests were enough to starve the pool and take the service down
//!   without anything that looked like an attack.
//! * **No explicit body limit.** The service was relying on axum's implicit 2 MB
//!   `DefaultBodyLimit`, which is invisible at the call site and is *not* what a sync
//!   payload full of drawing vector data was sized against. An implicit limit nobody
//!   chose is a limit nobody can reason about.
//! * **No concurrency ceiling.** Load simply queued. Rust does not fall over politely
//!   when a million futures are in flight: memory climbs, latency climbs with it, and
//!   the process is OOM-killed mid-transaction. Shedding early is strictly kinder than
//!   dying late.
//! * **No response security headers.** Nothing told a browser this host is HTTPS-only,
//!   that it must not sniff a content type, or that no response here belongs in a frame
//!   — and there is no shared place in front of this service that could. See
//!   [`security_headers`].
//! * **No panic guard.** A panicking handler unwound into hyper and killed the whole
//!   connection, taking any other in-flight request on it with it. This is not
//!   hypothetical: `auth::tokens::verify_refresh_token` calls `.expect(...)` on the
//!   stored hash, so one malformed row is one panic per refresh attempt.
//!
//! Every bound is environment-tunable, because the right number is an operational
//! question that changes with the instance size, and re-deploying a binary to change a
//! constant is how limits end up staying wrong.

pub mod security_headers;

use axum::{
    error_handling::HandleErrorLayer,
    extract::DefaultBodyLimit,
    http::StatusCode,
    response::{IntoResponse, Response},
    Router,
};
use std::time::Duration;
use tower::{limit::GlobalConcurrencyLimitLayer, load_shed::LoadShedLayer, ServiceBuilder};
use tower_http::{
    catch_panic::CatchPanicLayer, limit::RequestBodyLimitLayer, timeout::TimeoutLayer,
};

/// Longest a normal request may take before it is answered with `408`.
///
/// Well above the p99 of anything here (the slowest handler is a full sync against a
/// cold Neon instance) and well below the point at which a held connection matters.
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Largest request body accepted, in bytes.
///
/// Sized for sync, which is the only endpoint that carries bulk: a scribble's vector
/// data rides inside the change deltas, and a first sync from a tablet that has been
/// offline for a while batches many of them into one request. 8 MiB is roughly an order
/// of magnitude above the largest payload observed and four times axum's implicit
/// default, which is the limit this replaces.
const DEFAULT_MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Requests allowed to be in flight at once before the excess is shed with `503`.
///
/// The real ceiling downstream is the Postgres pool, which is far smaller; this exists
/// so that the queue in front of it is bounded and shed at a known depth rather than
/// growing until the allocator gives up.
const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 512;

/// The tunable half of the guardrails, read once at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guardrails {
    pub request_timeout: Duration,
    pub max_body_bytes: usize,
    pub max_concurrent_requests: usize,
    /// Lifetime advertised in `Strict-Transport-Security`. See
    /// [`security_headers::DEFAULT_HSTS_MAX_AGE_SECS`] for why it starts as small as it
    /// does and how it is meant to be ramped.
    pub hsts_max_age: Duration,
}

impl Default for Guardrails {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(DEFAULT_REQUEST_TIMEOUT_SECS),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_concurrent_requests: DEFAULT_MAX_CONCURRENT_REQUESTS,
            hsts_max_age: Duration::from_secs(security_headers::DEFAULT_HSTS_MAX_AGE_SECS),
        }
    }
}

impl Guardrails {
    /// Reads `REQUEST_TIMEOUT_SECS`, `MAX_REQUEST_BODY_BYTES`,
    /// `MAX_CONCURRENT_REQUESTS` and `HSTS_MAX_AGE_SECS`, falling back to the defaults
    /// above.
    ///
    /// A junk or zero value logs and falls back rather than panicking: a typo in a
    /// deployment manifest should not be the reason the service refuses to boot, and a
    /// zero ceiling would be indistinguishable from an outage.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            request_timeout: Duration::from_secs(env_positive(
                "REQUEST_TIMEOUT_SECS",
                DEFAULT_REQUEST_TIMEOUT_SECS,
            )),
            max_body_bytes: env_positive(
                "MAX_REQUEST_BODY_BYTES",
                defaults.max_body_bytes as u64,
            ) as usize,
            max_concurrent_requests: env_positive(
                "MAX_CONCURRENT_REQUESTS",
                defaults.max_concurrent_requests as u64,
            ) as usize,
            // Tunable for the same reason as the rest, and with one extra: the HSTS ramp
            // is a sequence of value changes held for weeks at a time, and doing that
            // through a manifest edit rather than a rebuild is what makes each step
            // cheap enough to actually take. `env_positive` refuses zero, so this knob
            // can lengthen or shorten the lifetime but not switch the header off; that
            // would be a code change, and deliberately so.
            hsts_max_age: Duration::from_secs(env_positive(
                "HSTS_MAX_AGE_SECS",
                security_headers::DEFAULT_HSTS_MAX_AGE_SECS,
            )),
        }
    }

    /// The request deadline, as a layer.
    ///
    /// Handed out separately rather than folded into [`Guardrails::apply`] because it is
    /// the one guardrail that must *not* cover every route — see the SSE note in
    /// `serve()`. Callers apply it to the sub-routers that should have a deadline.
    pub fn timeout_layer(&self) -> TimeoutLayer {
        // `TimeoutLayer::new` is deprecated in favour of naming the status explicitly;
        // 408 is what it used to send and what clients already retry on.
        TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, self.request_timeout)
    }

    /// Wraps `router` in the guardrails that belong on *every* route.
    ///
    /// Returned outermost-first, which is also the order a request meets them:
    ///
    /// 1. `SetResponseHeader` x5 — the response security headers. Outermost of
    ///    everything here on purpose: they must land on the responses no handler
    ///    produced as well as the ones one did — the shed `503`, the oversized `413`,
    ///    the caught `500`. They are also outermost of the CORS layer, which sits
    ///    further in, in `serve()`: these five header names and the `Access-Control-*`
    ///    set are disjoint, and every layer here is `if_not_present`, so a preflight
    ///    answered entirely by `CorsLayer` keeps every header it wrote and simply gains
    ///    five more. See [`security_headers`].
    /// 2. `HandleError` + `LoadShed` + `ConcurrencyLimit` — the ceiling. Load shedding is
    ///    what makes the ceiling a limit instead of a queue: without it a request over
    ///    the cap waits for a permit forever, which is unbounded queueing wearing a
    ///    limit's clothes. With it, the excess is refused immediately with `503` and the
    ///    client's own backoff does the rest. `HandleError` is needed because shedding
    ///    produces a tower error and axum routers are infallible.
    /// 3. `CatchPanic` — outside every handler and every layer below it, because a guard
    ///    that sits inside the thing it is guarding catches nothing. It is deliberately
    ///    *inside* the concurrency limit so that a panicking request still releases its
    ///    permit through the normal response path.
    /// 4. `RequestBodyLimit` — the body cap. Cheapest rejection in the stack, so it goes
    ///    above the handlers but below the ceiling; a request that is refused here never
    ///    reaches a handler at all.
    /// 5. `DefaultBodyLimit` — axum's extractor-side cap, pinned to the same number.
    ///
    /// Both body limits are set, and that is load-bearing rather than belt-and-braces.
    /// `routes::sync::types::AppJson` is a custom extractor built on
    /// `Bytes::from_request`, and `Bytes` enforces axum's `DefaultBodyLimit` — the
    /// implicit 2 MB one — from a request extension, *not* the tower layer. Setting only
    /// the tower layer would leave every `AppJson` body still capped at 2 MB with the
    /// real limit never reached; setting only `DefaultBodyLimit` would leave handlers
    /// that read the body by other means uncapped. Set together, an oversized body with
    /// a `Content-Length` — which is every real client here, since the Android side
    /// buffers its sync payload — is refused with `413` before the extractor runs. A
    /// chunked body has no length to check up front, so it is instead truncated at the
    /// limit and fails to parse, which `AppJson` reports as a deserialization error:
    /// the body is still bounded, only the status code is less precise.
    pub fn apply(&self, router: Router) -> Router {
        let bounded = router
            // `Router::layer` makes the *last* call the outermost, so this list reads
            // bottom-up: innermost first.
            .layer(DefaultBodyLimit::max(self.max_body_bytes))
            .layer(RequestBodyLimitLayer::new(self.max_body_bytes))
            .layer(CatchPanicLayer::custom(log_panic))
            .layer(
                // `ServiceBuilder` is the other way round — first call is outermost —
                // and these three have to be applied together because the load shedder
                // in the middle is the only thing here that can fail.
                ServiceBuilder::new()
                    .layer(HandleErrorLayer::new(shed_overload))
                    .layer(LoadShedLayer::new())
                    // `GlobalConcurrencyLimitLayer`, not `ConcurrencyLimitLayer`: the
                    // latter mints a fresh semaphore on every `layer()` call, and axum
                    // applies a router layer once per route, which would silently turn
                    // one global ceiling into a separate ceiling per endpoint. The
                    // global variant shares one `Arc<Semaphore>` across all of them.
                    //
                    // The permit is held for the response *future*, not the response
                    // *body*, which is why an idle SSE stream does not sit on one: the
                    // handler returns its `Sse` response promptly and streams after.
                    .layer(GlobalConcurrencyLimitLayer::new(self.max_concurrent_requests)),
            );

        security_headers::apply(bounded, self.hsts_max_age)
    }
}

/// Turns a shed request into a `503`. Anything else reaching here is a bug, so it is
/// logged loudly and reported as a `500`.
async fn shed_overload(err: tower::BoxError) -> Response {
    if err.is::<tower::load_shed::error::Overloaded>() {
        metrics::counter!("http_requests_shed_total").increment(1);
        tracing::warn!("Concurrency ceiling reached; shedding request");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({ "error": "Server overloaded, retry shortly" })),
        )
            .into_response();
    }

    tracing::error!("Unhandled middleware error: {}", err);
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(serde_json::json!({ "error": "Internal server error" })),
    )
        .into_response()
}

/// Panic handler for [`CatchPanicLayer`].
///
/// Custom rather than `CatchPanicLayer::new()` purely so the panic is *logged*: the
/// default handler answers `500` and says nothing, which converts a crash into a silent
/// error rate. The payload downcast covers the two shapes `panic!` produces — a
/// formatted `String` and a literal `&str` — and the message is logged, never returned,
/// because panic messages routinely carry internals.
fn log_panic(payload: Box<dyn std::any::Any + Send + 'static>) -> Response {
    let details = if let Some(message) = payload.downcast_ref::<String>() {
        message.as_str()
    } else if let Some(message) = payload.downcast_ref::<&'static str>() {
        message
    } else {
        "panic payload of unknown type"
    };

    metrics::counter!("http_handler_panics_total").increment(1);
    tracing::error!(panic = details, "Handler panicked; answering 500");

    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::Json(serde_json::json!({ "error": "Internal server error" })),
    )
        .into_response()
}

/// Reads a strictly positive integer from the environment, warning and falling back on
/// anything unusable. Zero is treated as unusable on purpose: a zero body limit or a
/// zero concurrency ceiling rejects all traffic, and an operator who typed it did not
/// mean "refuse everything".
fn env_positive(name: &str, default: u64) -> u64 {
    let raw = match std::env::var(name) {
        Ok(raw) => raw,
        Err(_) => return default,
    };

    match raw.trim().parse::<u64>() {
        Ok(value) if value > 0 => value,
        _ => {
            tracing::error!(
                "Ignoring unusable {}={:?}; falling back to {}",
                name,
                raw,
                default
            );
            default
        }
    }
}

#[cfg(test)]
mod tests;
