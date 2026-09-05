//! Response security headers: the small set of `Strict-Transport-Security`-shaped
//! instructions a browser only obeys if it is told, on every response.
//!
//! This service had none of them, and neither does anything in front of it. The shared
//! GKE Ingress applies a `FrontendConfig` with `redirectToHttps`, so a plaintext request
//! is bounced to HTTPS — but a redirect is an answer to a request that has *already been
//! sent in the clear*, which is exactly the request HSTS exists to prevent. GKE Ingress
//! has no response-header policy of its own, so there is no shared place to put these:
//! each backend sets its own or does without.
//!
//! Five headers, and the reason each one is here rather than "because a scanner asks for
//! it":
//!
//! * **`Strict-Transport-Security`** — closes the first-request-of-a-session hole above.
//!   The `max-age` is deliberately small; see [`DEFAULT_HSTS_MAX_AGE_SECS`].
//! * **`Content-Security-Policy: default-src 'none'`** — this policy would be reckless on
//!   a web app and is merely accurate here: nothing this service returns is ever rendered
//!   as a document. Every route answers with JSON, a bare `OK`-style string, or an SSE
//!   stream; there is no HTML, no template, no `ServeDir`, and no redirect to a page. A
//!   policy that forbids loading *any* subresource therefore forbids nothing that
//!   happens. What it buys is the case where that stops being true by accident — a JSON
//!   error string reflected into a response a browser is talked into treating as HTML —
//!   and `frame-ancestors 'none'` on top of it, which is the modern statement that no
//!   response here belongs in a frame.
//! * **`X-Content-Type-Options: nosniff`** — the other half of that: it stops a browser
//!   deciding for itself that `application/json` was really HTML or a script.
//! * **`X-Frame-Options: DENY`** — the superseded twin of `frame-ancestors`, kept because
//!   it costs 24 bytes and is the version an old embedded WebView actually implements.
//! * **`Referrer-Policy: no-referrer`** — the usual argument for the softer
//!   `strict-origin-when-cross-origin` is that a site wants its own referrers preserved
//!   for analytics. This serves no pages and has no analytics, so there is nothing to
//!   preserve and the strictest value is free. It matters at all only for the responses
//!   that *are* reached from a browser: `/auth/device/claim` is called by the `/link`
//!   page, and a URL of ours should not travel onward from there.
//!
//! Two decisions that are easy to get wrong and are load-bearing here:
//!
//! **Every layer is `if_not_present`, never `overriding`.** These are defaults for
//! responses that express no opinion, not corrections to responses that do. Nothing in
//! the tree sets any of the five today, so the two variants behave identically right now
//! — the difference only appears the day a handler deliberately sets one, and on that day
//! the handler, which knows what it is answering, should win rather than be silently
//! overwritten by a blanket layer. `overriding` would make that failure invisible.
//!
//! **Nothing here names `Content-Type`.** `/api/sync/stream` and `/api/v1/sync/stream`
//! return `text/event-stream` plus the proxy-compatibility headers from
//! `routes::sync::stream::build_sse_headers` (`Cache-Control: no-cache`, `Connection`,
//! `X-Accel-Buffering: no`), and every one of those survives untouched: this layer only
//! ever inserts header names that are not already present, and none of the five is in
//! that set. Nor does it touch the body — `SetResponseHeaderLayer` rewrites headers on
//! the way out and leaves the stream to stream.

use axum::{
    http::{header, HeaderValue},
    Router,
};
use std::time::Duration;
use tower_http::set_header::SetResponseHeaderLayer;

/// How long a browser is told to remember that this host is HTTPS-only. **One day.**
///
/// Small on purpose, and the smallness is the whole decision. HSTS is the one header
/// here that a client caches, which makes it the one that cannot be taken back by
/// deploying a fix: a browser that has recorded a year will refuse plaintext for that
/// host for a year whatever this service subsequently says. The failure it protects
/// against is real but modest; the failure it *causes* if this host ever has to serve
/// something over plain HTTP is total and lasts as long as the `max-age`. A day is long
/// enough to matter for a returning client and short enough that a mistake expires over
/// a weekend rather than a fiscal year.
///
/// The two options deliberately **not** taken:
///
/// * **No `includeSubDomains`.** `teddy.fyi` is not this service's domain to speak for.
///   The apex is an nginx site in another repo, `site-ingress` fronts both of us, and
///   neither this code nor anyone reading it knows what else answers under that name
///   today or will next month. `includeSubDomains` from an API subdomain is a
///   directive about hosts its author cannot enumerate — and it is enforced against
///   them for the full `max-age`.
/// * **No `preload`.** Preloading bakes the domain into browsers as shipped, which no
///   `max-age` can expire; removal is an out-of-band request to a list maintainer and a
///   wait measured in browser releases. It is a one-way door, and one-way doors are not
///   walked through on the same change that introduces the header.
///
/// The intended ramp, each step held long enough to be sure nothing broke: one day →
/// one week (`604800`) → six months (`15552000`) → one year (`31536000`), and only then
/// a separate, argued change for `includeSubDomains` once every host under the parent
/// domain is known to be HTTPS-only. Each step is a value change via `HSTS_MAX_AGE_SECS`
/// or this constant, not a code change, which is the point of it being a number.
pub const DEFAULT_HSTS_MAX_AGE_SECS: u64 = 86_400;

/// See the module docs. `frame-ancestors` rather than only `X-Frame-Options` because the
/// former is what current browsers read; `base-uri` and `form-action` are named
/// explicitly because `default-src` does not cover them, and a policy that leaves a gap
/// is worse than one that is obviously complete.
const CONTENT_SECURITY_POLICY: &str =
    "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

/// Builds the `Strict-Transport-Security` value for a given lifetime.
///
/// Fallible in principle and infallible in practice: the value is `max-age=` followed by
/// digits, so the only way it can fail to parse as a header is a change to this function.
fn hsts_value(max_age: Duration) -> HeaderValue {
    HeaderValue::try_from(format!("max-age={}", max_age.as_secs()))
        .expect("`max-age=<digits>` is always a valid header value")
}

/// Wraps `router` in the five header layers.
///
/// Applied by [`super::Guardrails::apply`] as the outermost thing in the guardrail
/// stack, so the headers are on *every* response — including the ones no handler
/// produced: the `503` from load shedding, the `413` from the body cap, the `500` from
/// the panic guard. A security header that is present only when the service is healthy
/// is a security header that is absent exactly when a client is being handed something
/// unusual.
///
/// It also covers routes that have no browser anywhere near them, `/healthz/*` included.
/// That is the cheaper mistake: kubelet ignores response headers entirely, so the cost
/// is a couple of hundred constant bytes on a probe, while the alternative — carving the
/// health routes out — buys nothing and creates a second class of route that a future
/// endpoint can be added to by accident and quietly lose its headers.
pub fn apply(router: Router, hsts_max_age: Duration) -> Router {
    router
        .layer(SetResponseHeaderLayer::if_not_present(
            header::STRICT_TRANSPORT_SECURITY,
            hsts_value(hsts_max_age),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(CONTENT_SECURITY_POLICY),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ))
}
