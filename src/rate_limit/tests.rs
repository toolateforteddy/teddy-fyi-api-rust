//! Behavioural tests for the auth rate limits.
//!
//! These drive a router through `tower::ServiceExt::oneshot` rather than a live socket, so
//! they exercise the real layer without a database, a port, or a sleep. The client IP is
//! whatever `X-Forwarded-For` says — which is precisely the header the deployed service
//! trusts — so each test can hand itself a private address and get a private bucket.

use crate::rate_limit::auth_limits::{
    layer, positive_from_env, Quota, AUTH_BURST, AUTH_REPLENISH_MS, DEVICE_START_BURST,
    DEVICE_START_REPLENISH_MS,
};
use crate::rate_limit::key::{ClientIpKeyExtractor, ClientKey};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;
use tower::ServiceExt;
use tower_governor::key_extractor::KeyExtractor;

/// A router shaped like the auth group: a general bucket over everything, a tighter one
/// on `/device/start`. Deliberately mirrors the wiring in `serve`, because the thing
/// under test is as much the stacking as the numbers.
fn app(general: Quota, device_start: Quota) -> Router {
    Router::new()
        .route(
            "/device/start",
            post(|| async { "ok" }).layer(layer(device_start.config())),
        )
        .route("/login", post(|| async { "ok" }))
        .route_layer(layer(general.config()))
}

fn request(path: &str, ip: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("x-forwarded-for", ip)
        .body(Body::empty())
        .unwrap()
}

/// Sends `count` requests from one address and returns the status of each.
async fn burst(app: &Router, path: &str, ip: &str, count: u32) -> Vec<StatusCode> {
    let mut statuses = Vec::new();
    for _ in 0..count {
        let response = app.clone().oneshot(request(path, ip)).await.unwrap();
        statuses.push(response.status());
    }
    statuses
}

/// A quota with a replenish period long enough that nothing comes back mid-test.
fn quota(burst: u32) -> Quota {
    Quota {
        burst,
        replenish: Duration::from_secs(3600),
    }
}

#[tokio::test]
async fn a_burst_inside_the_limit_is_served() {
    let app = app(quota(5), quota(5));

    let statuses = burst(&app, "/login", "203.0.113.1", 5).await;

    assert!(
        statuses.iter().all(|status| *status == StatusCode::OK),
        "every request inside the burst should be served, got {:?}",
        statuses
    );
}

#[tokio::test]
async fn a_burst_past_the_limit_is_throttled_with_retry_after() {
    let app = app(quota(3), quota(3));

    let statuses = burst(&app, "/login", "203.0.113.2", 3).await;
    assert!(statuses.iter().all(|status| *status == StatusCode::OK));

    let response = app
        .clone()
        .oneshot(request("/login", "203.0.113.2"))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    // A 429 with no `Retry-After` is a client's invitation to hot-loop, which is the
    // load we are trying to shed in the first place.
    assert!(
        response.headers().contains_key("retry-after"),
        "429 must tell the caller when to come back, headers were {:?}",
        response.headers()
    );
}

/// One noisy address must not throttle anybody else — the whole point of keying by IP.
#[tokio::test]
async fn buckets_are_per_address() {
    let app = app(quota(2), quota(2));

    let noisy = burst(&app, "/login", "203.0.113.3", 3).await;
    assert_eq!(noisy[2], StatusCode::TOO_MANY_REQUESTS);

    let response = app
        .clone()
        .oneshot(request("/login", "203.0.113.4"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// `/device/start` runs Argon2id before it authenticates anything, so its bucket has to
/// be the first to close — checked here against the shipped defaults, not test numbers.
#[tokio::test]
async fn device_start_is_tighter_than_the_rest_of_auth() {
    // Read through `Quota` rather than comparing the constants directly, so this asserts
    // the configuration the service actually resolves — environment overrides included.
    let general = Quota::general_auth();
    let start = Quota::device_start();
    assert!(
        start.burst < general.burst,
        "the expensive endpoint must not have the larger burst"
    );
    assert!(
        start.replenish > general.replenish,
        "the expensive endpoint must refill more slowly"
    );

    let app = app(general, start);
    let ip = "203.0.113.5";

    // Spend the device/start burst exactly.
    let statuses = burst(&app, "/device/start", ip, start.burst).await;
    assert!(
        statuses.iter().all(|status| *status == StatusCode::OK),
        "got {:?}",
        statuses
    );

    let throttled = app
        .clone()
        .oneshot(request("/device/start", ip))
        .await
        .unwrap();
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);

    // The general bucket is untouched by that: the same address can still log in, which
    // is what "tighter bucket on one route" has to mean.
    let login = app.clone().oneshot(request("/login", ip)).await.unwrap();
    assert_eq!(login.status(), StatusCode::OK);
}

/// Requests with no usable forwarding header share one bucket instead of erroring; a
/// `500` would be a worse answer than metering them.
#[test]
fn requests_with_no_client_ip_share_one_bucket() {
    let bare = Request::builder()
        .uri("/login")
        .body(Body::empty())
        .unwrap();
    let garbage = Request::builder()
        .uri("/login")
        .header("x-forwarded-for", "not-an-ip")
        .body(Body::empty())
        .unwrap();

    assert_eq!(
        ClientIpKeyExtractor.extract(&bare).unwrap(),
        ClientKey::Unattributed
    );
    assert_eq!(
        ClientIpKeyExtractor.extract(&garbage).unwrap(),
        ClientKey::Unattributed
    );
}

#[test]
fn the_forwarded_client_ip_is_the_key() {
    let req = Request::builder()
        .uri("/login")
        // Ingress appends, so the client is the leftmost entry.
        .header("x-forwarded-for", "198.51.100.7, 10.0.0.1")
        .body(Body::empty())
        .unwrap();

    assert_eq!(
        ClientIpKeyExtractor.extract(&req).unwrap(),
        ClientKey::Ip(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)))
    );
}

/// The knob an incident turns: a value in the environment wins, and an unusable one
/// falls back to the default rather than taking the limiter out.
#[test]
fn env_overrides_win_and_nonsense_falls_back() {
    // Uniquely named so this cannot collide with another test in the same process.
    const NAME: &str = "RATE_LIMIT_TEST_ONLY_KNOB";

    assert_eq!(positive_from_env(NAME, 7), 7);

    std::env::set_var(NAME, "42");
    assert_eq!(positive_from_env(NAME, 7), 42);

    // Whitespace is what a hand-edited manifest actually produces.
    std::env::set_var(NAME, " 9 ");
    assert_eq!(positive_from_env(NAME, 7), 9);

    // "0" reads like "off" but would mean "block everything", so it is refused.
    std::env::set_var(NAME, "0");
    assert_eq!(positive_from_env(NAME, 7), 7);

    std::env::set_var(NAME, "banana");
    assert_eq!(positive_from_env(NAME, 7), 7);

    std::env::remove_var(NAME);
}

/// The defaults must survive an empty environment, since that is how every deployment
/// runs until somebody turns a knob during an incident.
#[test]
fn defaults_apply_when_nothing_is_configured() {
    // Not `std::env::set_var`: tests share a process, and an override set here would
    // leak into the behavioural tests above. This only asserts the compiled defaults.
    assert_eq!(AUTH_BURST, 30);
    assert_eq!(AUTH_REPLENISH_MS, 500);
    assert_eq!(DEVICE_START_BURST, 5);
    assert_eq!(DEVICE_START_REPLENISH_MS, 15_000);
}
