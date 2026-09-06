use super::Guardrails;
use crate::routes::sync::AppJson;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::{get, post},
    Router,
};
use std::time::Duration;
use tower::ServiceExt;

/// Guardrails tightened to test scale. The numbers are arbitrary; what the tests care
/// about is that the layers are wired at all, and a 30-second deadline would make the
/// timeout tests take 30 seconds.
fn tiny() -> Guardrails {
    Guardrails {
        request_timeout: Duration::from_millis(50),
        max_body_bytes: 64,
        max_concurrent_requests: 8,
        // Deliberately not the production value, so the header tests below prove the
        // configured lifetime is what reaches the wire rather than a hard-coded string.
        hsts_max_age: Duration::from_secs(120),
    }
}

/// A body big enough to be refused by [`tiny`]'s limit, as a JSON document so it is the
/// *limit* being tested and not the JSON parser.
fn oversized_json() -> String {
    format!("{{\"note\":\"{}\"}}", "x".repeat(512))
}

#[tokio::test]
async fn oversized_body_is_refused_with_413_before_the_extractor_runs() {
    // `AppJson` deliberately, not `axum::Json`: it is a bespoke extractor over
    // `Bytes::from_request`, and it is the one every sync endpoint uses. A limit that
    // governed `Json` but not this would protect nothing that matters.
    async fn echo(AppJson(value): AppJson<serde_json::Value>) -> String {
        value.to_string()
    }

    let app = tiny().apply(Router::new().route("/echo", post(echo)));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/echo")
                .header("content-type", "application/json")
                // Set explicitly, because `Request::builder` does not. It is the first
                // thing the limit reads, and it is what a real client always sends:
                // OkHttp on the Android side buffers the sync payload and gives it a
                // length.
                .header("content-length", oversized_json().len().to_string())
                .body(Body::from(oversized_json()))
                .expect("request builds"),
        )
        .await
        .expect("router is infallible");

    assert_eq!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "a body over the limit must be refused, not parsed"
    );
}

#[tokio::test]
async fn a_body_under_the_limit_still_reaches_the_handler() {
    // The other half of the previous test: a limit that rejects everything would pass
    // it just as happily.
    async fn echo(AppJson(value): AppJson<serde_json::Value>) -> String {
        value.to_string()
    }

    let app = tiny().apply(Router::new().route("/echo", post(echo)));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/echo")
                .header("content-type", "application/json")
                .header("content-length", r#"{"note":"ok"}"#.len().to_string())
                .body(Body::from(r#"{"note":"ok"}"#))
                .expect("request builds"),
        )
        .await
        .expect("router is infallible");

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_panicking_handler_answers_500_instead_of_unwinding() {
    // Without `CatchPanicLayer` this panic escapes into hyper and kills the connection,
    // taking every other in-flight request on it down as well. The example that motivated
    // the layer was `auth::tokens::verify_refresh_token`, which `.expect(...)`ed on a
    // stored hash; that one has since been removed, which is why this test panics on its
    // own rather than through a real handler.
    //
    // The panic message is printed to stderr by the default hook while this test runs.
    // That is the test working, not the test failing.
    async fn boom() -> &'static str {
        panic!("invalid hash format");
    }

    let app = tiny().apply(Router::new().route("/boom", get(boom)));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/boom")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("the panic guard makes this infallible");

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

/// A router shaped exactly like `serve()`'s: slow endpoints under the deadline, and the
/// SSE endpoints merged in afterwards so they never meet it.
fn router_with_a_deadlined_half_and_an_sse_half(guardrails: Guardrails) -> Router {
    // Comfortably longer than `tiny()`'s 50ms deadline, and short enough that the
    // exemption test pays it in real time without anyone noticing.
    async fn slow() -> &'static str {
        tokio::time::sleep(Duration::from_millis(400)).await;
        "eventually"
    }

    let stream_routes = Router::new().route("/sync/stream", get(slow));

    Router::new()
        .route("/sync", post(slow))
        .layer(guardrails.timeout_layer())
        .merge(stream_routes)
}

#[tokio::test]
async fn an_ordinary_route_is_cut_off_by_the_request_deadline() {
    let app = router_with_a_deadlined_half_and_an_sse_half(tiny());

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/sync")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router is infallible");

    assert_eq!(
        response.status(),
        StatusCode::REQUEST_TIMEOUT,
        "a stalled request must not hold its handler, and its database connection, forever"
    );
}

#[tokio::test]
async fn the_sse_route_is_not_subject_to_the_request_deadline() {
    // The guarantee this test exists to hold: `/api/sync/stream` and
    // `/api/v1/sync/stream` stay open for the life of the app foreground, with a
    // 240-second keep-alive between pings. Anything that hands them a 30-second
    // deadline turns real-time sync into polling for every client at once.
    //
    // The stand-in stream handler takes eight times the deadline to answer. If the SSE
    // half ever acquires the timeout layer — by being moved back into the router that
    // carries it — this returns 408 and the test fails.
    let app = router_with_a_deadlined_half_and_an_sse_half(tiny());

    let response = app
        .oneshot(
            Request::builder()
                .uri("/sync/stream")
                .body(Body::empty())
                .expect("request builds"),
        )
        .await
        .expect("router is infallible");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "the SSE routes must outlive the deadline that governs everything else"
    );
}

mod config {
    use super::super::{env_positive, Guardrails};
    use std::time::Duration;

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let defaults = Guardrails::default();
        assert_eq!(defaults.request_timeout, Duration::from_secs(30));
        assert_eq!(defaults.max_body_bytes, 8 * 1024 * 1024);
        assert_eq!(defaults.max_concurrent_requests, 512);
        assert_eq!(defaults.hsts_max_age, Duration::from_secs(86_400));
    }

    #[test]
    fn a_value_that_is_set_is_used() {
        // Unique variable name per test: `cargo test` runs these in one process, so a
        // shared name would race with the other cases here.
        std::env::set_var("GUARDRAIL_TEST_PARSES", "17");
        assert_eq!(env_positive("GUARDRAIL_TEST_PARSES", 3), 17);
        std::env::remove_var("GUARDRAIL_TEST_PARSES");
    }

    #[test]
    fn junk_and_zero_fall_back_rather_than_panicking() {
        // A typo in a deployment manifest must cost the default, not the process; and
        // zero must not be honoured, because a zero ceiling refuses all traffic and no
        // operator ever means that.
        std::env::set_var("GUARDRAIL_TEST_JUNK", "thirty");
        assert_eq!(env_positive("GUARDRAIL_TEST_JUNK", 30), 30);
        std::env::set_var("GUARDRAIL_TEST_JUNK", "0");
        assert_eq!(env_positive("GUARDRAIL_TEST_JUNK", 30), 30);
        std::env::remove_var("GUARDRAIL_TEST_JUNK");

        assert_eq!(env_positive("GUARDRAIL_TEST_UNSET", 42), 42);
    }
}

/// The response security headers.
///
/// Every case here asserts on a header of a *real* response through the real
/// [`Guardrails::apply`] stack, because the failure mode being guarded against is the
/// layer being present in the source and absent from the wire — wrong variant, wrong
/// place in the stack, or clobbered by something further out.
mod security_headers {
    use super::tiny;
    use crate::guardrails::security_headers::DEFAULT_HSTS_MAX_AGE_SECS;
    use crate::routes::sync::stream::build_sse_headers;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        response::sse::{Event, Sse},
        response::IntoResponse,
        routing::{get, post},
        Router,
    };
    use std::convert::Infallible;
    use tower::ServiceExt;

    /// Reads a header as a string, failing the test by name if it is missing.
    fn header(response: &axum::response::Response, name: &str) -> String {
        response
            .headers()
            .get(name)
            .unwrap_or_else(|| panic!("response is missing the `{}` header", name))
            .to_str()
            .expect("header value is ASCII")
            .to_string()
    }

    fn assert_security_headers(response: &axum::response::Response) {
        assert_eq!(header(response, "x-content-type-options"), "nosniff");
        assert_eq!(header(response, "x-frame-options"), "DENY");
        assert_eq!(header(response, "referrer-policy"), "no-referrer");
        assert_eq!(
            header(response, "content-security-policy"),
            "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"
        );
        // `tiny()`'s lifetime, not the default one: this is what proves the configured
        // value is plumbed through rather than a constant baked into the layer.
        assert_eq!(
            header(response, "strict-transport-security"),
            "max-age=120"
        );
    }

    #[tokio::test]
    async fn an_ordinary_json_response_carries_every_header() {
        async fn json() -> axum::Json<serde_json::Value> {
            axum::Json(serde_json::json!({ "ok": true }))
        }

        let app = tiny().apply(Router::new().route("/thing", get(json)));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/thing")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router is infallible");

        assert_eq!(response.status(), StatusCode::OK);
        assert_security_headers(&response);
    }

    #[tokio::test]
    async fn the_sse_response_keeps_its_own_content_type_and_headers() {
        // The guarantee: `if_not_present` layers add to an SSE response and take nothing
        // away from it. `text/event-stream` is set by `Sse`'s `IntoResponse`, and the
        // three proxy-compatibility headers are the real `build_sse_headers` the stream
        // handler uses — an `overriding` layer over any of them would break real-time
        // sync silently, which is why this asserts them rather than only the additions.
        async fn stream() -> impl IntoResponse {
            let events = futures_util::stream::once(async {
                Ok::<_, Infallible>(Event::default().data("hello"))
            });
            (build_sse_headers(), Sse::new(events))
        }

        let app = tiny().apply(Router::new().route("/sync/stream", get(stream)));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/sync/stream")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router is infallible");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            header(&response, "content-type").starts_with("text/event-stream"),
            "the stream must still be served as an event stream, not as whatever a \
             header layer left behind: {}",
            header(&response, "content-type")
        );
        assert_eq!(header(&response, "cache-control"), "no-cache");
        assert_eq!(header(&response, "x-accel-buffering"), "no");
        assert_security_headers(&response);
    }

    #[tokio::test]
    async fn a_response_no_handler_produced_carries_them_too() {
        // The 413 from the body cap never reaches a handler, which is exactly why it is
        // worth asserting: it proves the header layers sit outside the rest of the
        // guardrails rather than inside them.
        async fn never_runs(_body: String) -> &'static str {
            "unreachable"
        }

        let app = tiny().apply(Router::new().route("/echo", post(never_runs)));
        let body = "x".repeat(512);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/echo")
                    .header("content-type", "text/plain")
                    .header("content-length", body.len().to_string())
                    .body(Body::from(body))
                    .expect("request builds"),
            )
            .await
            .expect("router is infallible");

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_security_headers(&response);
    }

    #[tokio::test]
    async fn a_cors_preflight_keeps_its_cors_headers_and_gains_these() {
        // The header layers are applied outside the CORS layer, and a preflight is
        // answered by `CorsLayer` alone — no handler runs. If the ordering interfered,
        // this is where it would show: either the `Access-Control-*` set goes missing or
        // the security headers do.
        let cors = tower_http::cors::CorsLayer::new()
            .allow_origin(crate::allowed_origins())
            .allow_credentials(true)
            .allow_methods([axum::http::Method::POST, axum::http::Method::OPTIONS])
            .allow_headers([axum::http::header::CONTENT_TYPE]);

        let app = tiny().apply(
            Router::new()
                .route("/sync", post(|| async { "ok" }))
                .layer(cors),
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri("/sync")
                    .header("origin", "https://teddy.fyi")
                    .header("access-control-request-method", "POST")
                    .header("access-control-request-headers", "content-type")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("router is infallible");

        assert_eq!(
            header(&response, "access-control-allow-origin"),
            "https://teddy.fyi",
            "the preflight must still be answered, or browser clients cannot call this API"
        );
        assert_eq!(header(&response, "access-control-allow-credentials"), "true");
        assert_security_headers(&response);
    }

    #[test]
    fn the_shipped_hsts_lifetime_is_the_conservative_one() {
        // This is the value that goes to production, and it is the one decision here
        // that a browser caches and this service cannot take back. A change to it should
        // be a deliberate step of the ramp documented on the constant, which means
        // changing this assertion too — not something that rides along with an unrelated
        // edit.
        assert_eq!(DEFAULT_HSTS_MAX_AGE_SECS, 86_400, "one day");

        let shipped = crate::guardrails::Guardrails::default();
        let value = format!("max-age={}", shipped.hsts_max_age.as_secs());
        assert_eq!(value, "max-age=86400");
        assert!(
            !value.contains("includeSubDomains"),
            "this service does not speak for the other hosts under its parent domain"
        );
        assert!(
            !value.contains("preload"),
            "preload is a one-way door and is not taken on the change that introduces HSTS"
        );
    }
}
