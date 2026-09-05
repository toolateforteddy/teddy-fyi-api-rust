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
    // taking every other in-flight request on it down as well. The live example is
    // `auth::tokens::verify_refresh_token`, which `.expect(...)`s on a stored hash.
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
