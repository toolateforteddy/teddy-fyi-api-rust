use crate::routes::sync::publisher::{get_channel_name, get_device_channel_name, SyncSseEvent};
use crate::routes::sync::stream::{build_sse_headers, event_targets_device};
use crate::routes::sync::config::fetch_config_snapshot;
use crate::routes::sync::tests::helpers::{seed_device, setup_state, sync_handler};
use crate::routes::sync::{AppJson, ConfigSyncItem, SyncRequest, SyncScope, parse_or_hash_uuid};
use axum::extract::State;
use axum::http::header;
use chrono::Utc;
use futures_util::StreamExt;
use serde_json::json;
use sqlx::PgPool;

#[test]
fn test_sync_sse_event_direct_update_serialization() {
    let event = SyncSseEvent::DirectUpdate {
        entity: "flags".to_string(),
        key: "volume".to_string(),
        value: json!(85),
        sender_client_id: Some("client-123".to_string()),
        device_uuid: None,
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
        device_uuid: None,
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
        device_uuid: None,
    };

    let remote_event = SyncSseEvent::DirectUpdate {
        entity: "flags".to_string(),
        key: "volume".to_string(),
        value: json!(90),
        sender_client_id: Some("client-xyz".to_string()),
        device_uuid: None,
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

#[test]
fn test_device_channel_name_formatting() {
    let device = uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
    let channel = get_device_channel_name("usr_abc123", &device);
    assert_eq!(
        channel,
        "sync_channel:usr_abc123:device:11111111-2222-3333-4444-555555555555"
    );
}

#[test]
fn test_event_targets_device_filtering() {
    let device = uuid::Uuid::new_v4();
    let other_device = uuid::Uuid::new_v4();

    let for_device = SyncSseEvent::DirectUpdate {
        entity: "config".to_string(),
        key: "theme".to_string(),
        value: json!("dark"),
        sender_client_id: None,
        device_uuid: Some(device),
    };
    let account_wide = SyncSseEvent::Invalidate {
        entity: "config".to_string(),
        sender_client_id: None,
        device_uuid: None,
    };

    assert!(event_targets_device(&for_device, Some(device)));
    assert!(!event_targets_device(&for_device, Some(other_device)));
    assert!(
        !event_targets_device(&for_device, None),
        "a stream that named no device should not receive another tablet's config"
    );
    assert!(event_targets_device(&account_wide, Some(device)));
    assert!(event_targets_device(&account_wide, None));
}

/// A ScribbleKeep Cloud config write should land on the device's Pub/Sub channel — the one
/// the SSE stream subscribes to when it is given that device id.
#[sqlx::test]
async fn test_config_write_publishes_to_device_channel(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;

    let mut pubsub = match state.redis_client.get_async_pubsub().await {
        Ok(pubsub) => pubsub,
        Err(err) => {
            eprintln!("SKIPPING test_config_write_publishes_to_device_channel: no Redis ({err})");
            return;
        }
    };
    pubsub
        .subscribe(&get_device_channel_name("user-1", &device_uuid))
        .await
        .unwrap();
    let mut messages = pubsub.on_message();

    let config_id = uuid::Uuid::new_v4();
    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        client_id: "client-1".to_string(),
        device_uuid: Some(device_uuid),
        device_name: None,
        scope: Some(SyncScope::ScribbleKeepCloud),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![ConfigSyncItem {
            id: config_id,
            device_uuid: Some(device_uuid),
            key: "theme".to_string(),
            value: "dark".to_string(),
            sync_state: "PENDING_INSERT".to_string(),
            version: 1,
            is_deleted: false,
            last_modified: Utc::now().timestamp_millis(),
        }],
        drawings: vec![],
    };

    let _ = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed");

    let msg = tokio::time::timeout(std::time::Duration::from_secs(5), messages.next())
        .await
        .expect("Timed out waiting for the config event on the device channel")
        .expect("Pub/Sub stream ended");

    let payload: String = msg.get_payload().unwrap();
    let event: SyncSseEvent = serde_json::from_str(&payload).unwrap();

    match event {
        SyncSseEvent::DirectUpdate {
            entity,
            key,
            value,
            sender_client_id,
            device_uuid: event_device,
        } => {
            assert_eq!(entity, "config");
            assert_eq!(key, "theme");
            assert_eq!(value["value"], "dark");
            assert_eq!(value["id"], config_id.to_string());
            assert_eq!(sender_client_id.as_deref(), Some("client-1"));
            assert_eq!(event_device, Some(device_uuid));
        }
        other => panic!("Expected a DIRECT_UPDATE for the config, got {other:?}"),
    }
}

/// The stream opens with the device's live configs, not a placeholder.
#[sqlx::test]
async fn test_config_snapshot_is_device_scoped(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let client_uuid = parse_or_hash_uuid("client-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;
    let other_device = seed_device(&pool, user_uuid, "Other Tablet").await;

    let insert = |id: uuid::Uuid, device: uuid::Uuid, key: &'static str, deleted: bool| {
        let pool = pool.clone();
        async move {
            sqlx::query!(
                "INSERT INTO configs (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state, key, value) \
                 VALUES ($1, $2, $3, $4, 1, $5, $6, 'SYNCED'::sync_state, $7, 'dark')",
                id,
                user_uuid,
                device,
                client_uuid,
                deleted,
                Utc::now().timestamp_millis(),
                key
            )
            .execute(&pool)
            .await
            .unwrap();
        }
    };

    let live = uuid::Uuid::new_v4();
    let deleted = uuid::Uuid::new_v4();
    let elsewhere = uuid::Uuid::new_v4();
    insert(live, device_uuid, "theme", false).await;
    insert(deleted, device_uuid, "font_size", true).await;
    insert(elsewhere, other_device, "theme", false).await;

    let scoped = fetch_config_snapshot(&pool, &user_uuid, Some(device_uuid))
        .await
        .unwrap();
    let scoped_ids: Vec<uuid::Uuid> = scoped.iter().map(|c| c.id).collect();
    assert_eq!(scoped_ids, vec![live], "only the named device's live configs");
    assert_eq!(scoped[0].key, "theme");
    assert_eq!(scoped[0].value, "dark");
    assert_eq!(scoped[0].device_uuid, Some(device_uuid));

    // No device named: every live config on the account, still no deleted rows.
    let account_wide = fetch_config_snapshot(&pool, &user_uuid, None).await.unwrap();
    let mut ids: Vec<uuid::Uuid> = account_wide.iter().map(|c| c.id).collect();
    ids.sort();
    let mut expected = vec![live, elsewhere];
    expected.sort();
    assert_eq!(ids, expected);
    assert!(!account_wide.iter().any(|c| c.id == deleted));
}
