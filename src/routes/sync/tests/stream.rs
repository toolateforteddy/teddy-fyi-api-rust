use crate::routes::sync::publisher::{get_channel_name, SyncSseEvent};
use crate::routes::sync::stream::build_sse_headers;
use axum::http::header;
use serde_json::json;

#[test]
fn test_sync_sse_event_direct_update_serialization() {
    let event = SyncSseEvent::DirectUpdate {
        entity: "flags".to_string(),
        key: "volume".to_string(),
        value: json!(85),
        sender_client_id: Some("client-123".to_string()),
    };

    let json_str = serde_json::to_string(&event).expect("Serialization failed");
    let val: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    assert_eq!(val["type"], "DIRECT_UPDATE");
    assert_eq!(val["entity"], "flags");
    assert_eq!(val["key"], "volume");
    assert_eq!(val["value"], 85);
    assert_eq!(val["sender_client_id"], "client-123");
}

#[test]
fn test_sync_sse_event_invalidate_serialization() {
    let event = SyncSseEvent::Invalidate {
        entity: "user_settings".to_string(),
        sender_client_id: Some("client-456".to_string()),
    };

    let json_str = serde_json::to_string(&event).expect("Serialization failed");
    let val: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    assert_eq!(val["type"], "INVALIDATE");
    assert_eq!(val["entity"], "user_settings");
    assert_eq!(val["sender_client_id"], "client-456");
}

#[test]
fn test_sync_channel_name_formatting() {
    let channel = get_channel_name("usr_abc123");
    assert_eq!(channel, "sync_channel:usr_abc123");
}

#[test]
fn test_sse_headers_configuration() {
    let headers = build_sse_headers();

    assert_eq!(
        headers.get(header::CACHE_CONTROL).unwrap().to_str().unwrap(),
        "no-cache"
    );
    assert_eq!(
        headers.get(header::CONNECTION).unwrap().to_str().unwrap(),
        "keep-alive"
    );
    assert_eq!(
        headers.get("x-accel-buffering").unwrap().to_str().unwrap(),
        "no"
    );
}

#[test]
fn test_echo_filtering_logic() {
    let current_client = "client-abc";
    
    let echo_event = SyncSseEvent::DirectUpdate {
        entity: "flags".to_string(),
        key: "volume".to_string(),
        value: json!(90),
        sender_client_id: Some("client-abc".to_string()),
    };

    let remote_event = SyncSseEvent::DirectUpdate {
        entity: "flags".to_string(),
        key: "volume".to_string(),
        value: json!(90),
        sender_client_id: Some("client-xyz".to_string()),
    };

    // Check echo match
    let is_echo = match &echo_event {
        SyncSseEvent::DirectUpdate { sender_client_id, .. } => {
            sender_client_id.as_deref() == Some(current_client)
        }
        _ => false,
    };
    assert!(is_echo, "Event from same client_id should be identified as echo");

    // Check remote match
    let is_remote_echo = match &remote_event {
        SyncSseEvent::DirectUpdate { sender_client_id, .. } => {
            sender_client_id.as_deref() == Some(current_client)
        }
        _ => false,
    };
    assert!(!is_remote_echo, "Event from different client_id should not be echo");
}
