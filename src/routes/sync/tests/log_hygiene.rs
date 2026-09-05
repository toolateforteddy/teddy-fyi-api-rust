//! Guards on what the sync path is allowed to write into the logs.
//!
//! Cloud Logging is reachable by neither `DELETE /api/user/data` nor
//! `jobs::reap_stale_users`, so a log line carrying a request body or a config
//! value is a copy of user data that no erasure path can ever delete. These tests
//! assert on the *emitted* tracing events, not on the call sites, so a future
//! edit that puts the payload back fails here rather than in production.

use crate::routes::sync::publisher::{log_published, SyncSseEvent};
use crate::routes::sync::types::{describe_rejected_body, truncate_parse_error, AppJson};
use crate::routes::sync::SyncRequest;
use axum::extract::FromRequest;
use std::io;
use std::sync::{Arc, Mutex};
use tracing_subscriber::fmt::MakeWriter;

/// Collects everything a `tracing` subscriber writes, so a test can read back the
/// exact bytes that would have gone to Cloud Logging.
#[derive(Clone, Default)]
struct CapturedLog(Arc<Mutex<Vec<u8>>>);

impl CapturedLog {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("log mutex")).to_string()
    }
}

impl io::Write for CapturedLog {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("log mutex").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for CapturedLog {
    type Writer = CapturedLog;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Runs `body` with every tracing event captured. Scoped rather than global:
/// `set_global_default` can only be called once per process, and these tests share
/// one with the rest of the suite.
fn capture_logs<T>(body: impl FnOnce() -> T) -> (T, String) {
    let sink = CapturedLog::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(sink.clone())
        .with_ansi(false)
        // Everything, so a test can never pass merely because the line was filtered.
        .with_max_level(tracing::Level::TRACE)
        .finish();

    let value = tracing::subscriber::with_default(subscriber, body);
    (value, sink.contents())
}

/// A string that appears nowhere except in the payload under test, so finding it in
/// the log output can only mean the payload was echoed.
const CANARY: &str = "sooper-secret-drawing-stroke";

#[tokio::test]
async fn malformed_body_is_never_echoed_into_the_log() {
    // Shaped like a real sync upload — a config value and a drawing blob — but with
    // `version` as a string, which is a type error serde will reject.
    let body = format!(
        r#"{{"client_id":"c1","configs":[{{"id":"not-a-uuid","key":"child_name","value":"{CANARY}","version":"1","is_deleted":false,"last_modified":0}}]}}"#
    );

    let (result, logs) = {
        let body = body.clone();
        let request = axum::http::Request::builder()
            .method("POST")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body))
            .expect("request builds");

        // The extractor is async, so the capture has to span the await rather than
        // wrap a plain closure.
        let sink = CapturedLog::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let result = AppJson::<SyncRequest>::from_request(request, &()).await;
        drop(guard);
        (result, sink.contents())
    };

    assert!(result.is_err(), "the payload must not parse, or this proves nothing");
    assert!(
        !logs.contains(CANARY),
        "request body content reached the log: {logs}"
    );
    // And the diagnostics that replaced it are actually there.
    assert!(logs.contains("json_rejected"), "missing event tag: {logs}");
    assert!(logs.contains("body_hash"), "missing body hash: {logs}");
    assert!(
        logs.contains(&format!("body_bytes={}", body.len())),
        "missing or wrong body length: {logs}"
    );
    // A client-caused 400 is not an operator-actionable error; see the call site.
    assert!(logs.contains("WARN"), "expected warn level: {logs}");
    assert!(!logs.contains("ERROR"), "parse failures must not page anyone: {logs}");
}

#[test]
fn body_hash_is_stable_for_identical_bodies_and_differs_otherwise() {
    let a = describe_rejected_body(b"{\"broken\":", "salt");
    let b = describe_rejected_body(b"{\"broken\":", "salt");
    let c = describe_rejected_body(b"{\"broken\"!", "salt");

    // The whole point of hashing rather than dropping the body: one client retrying
    // one bad request has to stay recognisable as a single fault.
    assert_eq!(a, b, "identical bodies must produce identical digests");
    assert_ne!(a.hash, c.hash, "different bodies must produce different digests");

    // Salted, so nobody holding the logs can confirm a guessed payload offline.
    let unsalted = describe_rejected_body(b"{\"broken\":", "other-salt");
    assert_ne!(a.hash, unsalted.hash, "the salt must change the digest");

    assert_eq!(a.len, 10);
    assert_eq!(a.hash.len(), 16);
    assert!(a.hash.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn parse_error_message_is_bounded() {
    let short = "missing field `client_id` at line 3 column 5";
    assert_eq!(truncate_parse_error(short), short, "useful messages pass through");

    // The disclosure case: serde quotes an offending string value verbatim, so an
    // attacker could otherwise drive unbounded log volume with one huge field.
    let huge = format!("invalid type: string \"{}\", expected i32", "x".repeat(10_000));
    let truncated = truncate_parse_error(&huge);
    assert!(truncated.len() < 240, "message not bounded: {}", truncated.len());
    assert!(truncated.ends_with("…(truncated)"));

    // Multi-byte characters must not be split mid-character.
    let multibyte = format!("invalid type: string \"{}\", expected i32", "é".repeat(5_000));
    let _ = truncate_parse_error(&multibyte);
}

#[test]
fn serde_messages_for_realistic_failures_name_the_field_not_the_data() {
    // The claim the redaction rests on: for the common failure shapes the message
    // is structural. Held here so a serde_json upgrade that starts quoting payloads
    // is caught rather than assumed away.
    let missing = serde_json::from_str::<SyncRequest>(r#"{"scope":"ALL"}"#)
        .expect_err("client_id is required");
    assert!(missing.to_string().contains("client_id"), "{missing}");

    let wrong_shape = serde_json::from_str::<SyncRequest>(
        &format!(r#"{{"client_id":{{"nested":"{CANARY}"}}}}"#),
    )
    .expect_err("client_id must be a string");
    assert!(
        !wrong_shape.to_string().contains(CANARY),
        "serde quoted the payload: {wrong_shape}"
    );
    assert!(wrong_shape.to_string().contains("line"), "{wrong_shape}");
}

#[test]
fn a_type_mismatch_does_quote_the_offending_value() {
    // The counter-example, and the reason the message is length-bounded rather than
    // trusted: when a scalar is the wrong type, serde renders the value itself into
    // the message — `invalid type: string "<value>", expected i32`. So the error
    // text is not unconditionally free of user data; it is only bounded. Anything
    // stricter (dropping the message) would cost the field name and offset that
    // make a client bug diagnosable at all, which is a bad trade for a fragment of
    // one already-malformed field.
    let payload = format!(
        r#"{{"client_id":"c1","configs":[{{"id":"00000000-0000-0000-0000-000000000000","key":"k","value":"v","version":"{CANARY}","is_deleted":false,"last_modified":0}}]}}"#
    );
    let err = serde_json::from_str::<SyncRequest>(&payload).expect_err("version is an i32");
    assert!(
        err.to_string().contains(CANARY),
        "assumption changed — serde no longer quotes the value: {err}"
    );
    assert!(err.to_string().contains("invalid type"), "{err}");
}

#[test]
fn publish_log_carries_no_config_value() {
    let event = SyncSseEvent::DirectUpdate {
        entity: "config".to_string(),
        key: "child_name".to_string(),
        value: serde_json::json!(CANARY),
        sender_client_id: Some("client-1".to_string()),
        device_uuid: None,
        is_deleted: false,
    };

    let (_, logs) = capture_logs(|| log_published("user", "user-42", None, &event));

    assert!(!logs.contains(CANARY), "config value reached the log: {logs}");
    // The raw user id goes the same way as the value; only the hash is logged.
    assert!(!logs.contains("user-42"), "raw user id reached the log: {logs}");

    // Still enough left to answer "did the fan-out happen, and about what".
    assert!(logs.contains("sse_published"), "{logs}");
    assert!(logs.contains("DIRECT_UPDATE"), "{logs}");
    assert!(logs.contains("config"), "{logs}");
    assert!(logs.contains("user_hash"), "{logs}");
}

#[test]
fn publish_log_keeps_the_device_and_variant() {
    let device = uuid::Uuid::nil();
    let event = SyncSseEvent::Invalidate {
        entity: "drawings".to_string(),
        sender_client_id: None,
        device_uuid: Some(device),
    };

    let (_, logs) = capture_logs(|| log_published("device", "user-42", Some(&device), &event));

    assert!(logs.contains("INVALIDATE"), "{logs}");
    assert!(logs.contains("drawings"), "{logs}");
    assert!(logs.contains(&device.to_string()), "{logs}");
}
