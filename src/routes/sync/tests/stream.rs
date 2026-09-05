use crate::routes::sync::publisher::{get_channel_name, get_device_channel_name, SyncSseEvent};
use crate::routes::sync::stream::{build_sse_headers, event_targets_device, resolve_stream_device};
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
        is_deleted: false,
    };

    let json_str = serde_json::to_string(&event).expect("Serialization failed");
    let val: serde_json::Value = serde_json::from_str(&json_str).unwrap();

    assert_eq!(val["type"], "DIRECT_UPDATE");
    assert_eq!(val["entity"], "flags");
    assert_eq!(val["key"], "volume");
    assert_eq!(val["value"], 85);
    assert_eq!(val["sender_client_id"], "client-123");
    assert!(
        val.get("is_deleted").is_none(),
        "an ordinary write should not carry a tombstone flag"
    );
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
        is_deleted: false,
    };

    let remote_event = SyncSseEvent::DirectUpdate {
        entity: "flags".to_string(),
        key: "volume".to_string(),
        value: json!(90),
        sender_client_id: Some("client-xyz".to_string()),
        device_uuid: None,
        is_deleted: false,
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
        is_deleted: false,
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
            is_deleted,
        } => {
            assert_eq!(entity, "config");
            assert_eq!(key, "theme");
            // The config's own value, not the serialized row: a listener stores `value`
            // verbatim, so anything else lands in its database as the setting.
            assert_eq!(value, serde_json::Value::String("dark".to_string()));
            assert_eq!(sender_client_id.as_deref(), Some("client-1"));
            assert_eq!(event_device, Some(device_uuid));
            assert!(!is_deleted);
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


/// A tablet that opens the stream without naming a device still lands on the device its
/// own sync writes go to — the account's fallback — so the config events published there
/// reach it. Without this the stream sits on the account-wide channel and hears nothing.
#[sqlx::test]
async fn test_stream_without_device_falls_back_to_account_device(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;
    // A second, newer device must not steal the fallback from the backfilled one.
    seed_device(&pool, user_uuid, "Other Tablet").await;

    let resolved = resolve_stream_device(&pool, &user_uuid, None, Some("scribble_keep"))
        .await
        .unwrap();
    assert_eq!(
        resolved,
        Some(device_uuid),
        "a device-less tablet stream should watch the same device its sync resolves to"
    );

    // An explicit device always wins over the fallback.
    let named = uuid::Uuid::new_v4();
    let resolved = resolve_stream_device(&pool, &user_uuid, Some(named), Some("scribble_keep"))
        .await
        .unwrap();
    assert_eq!(resolved, Some(named));
}

/// The cloud dashboard watches every tablet on the account, so naming no device leaves it
/// account-wide rather than pinning it to one — the same split the sync endpoint draws.
#[sqlx::test]
async fn test_cloud_stream_without_device_stays_account_wide(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;

    for scope in ["SCRIBBLE_KEEP_CLOUD", "scribble_keep_cloud"] {
        let resolved = resolve_stream_device(&pool, &user_uuid, None, Some(scope))
            .await
            .unwrap();
        assert_eq!(resolved, None, "cloud scope {scope} should stay account-wide");
    }
}

/// Connecting is a read: an account with no device yet gets an account-wide stream, not a
/// freshly minted device row.
#[sqlx::test]
async fn test_stream_does_not_register_a_device(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");

    let resolved = resolve_stream_device(&pool, &user_uuid, None, Some("scribble_keep"))
        .await
        .unwrap();
    assert_eq!(resolved, None);

    let devices = sqlx::query_scalar!("SELECT COUNT(*) FROM devices WHERE user_id = $1", user_uuid)
        .fetch_one(&pool)
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(devices, 0, "opening a stream must not register a device");
}

// --- Stream concurrency caps -------------------------------------------------
//
// The unit tests below drive `StreamSlots` directly: slot accounting is pure, and
// these are the assertions that have to be deterministic. The two handler tests
// after them exist to pin the *status codes*, which is the part a client depends
// on — and they need no Redis, because a refused stream is refused before the
// subscribe.

use crate::auth::tokens::Claims;
use crate::routes::sync::stream::{sync_stream_handler, StreamQuery};
use crate::routes::sync::stream_limits::{StreamRefusal, StreamSlots};
use crate::routes::sync::AppError;
use crate::state::AppState;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use std::sync::Arc;

/// One family's tablets connect; the loop that tries to open a fourth is refused.
#[test]
fn test_per_user_cap_admits_up_to_the_limit_then_refuses() {
    let slots = Arc::new(StreamSlots::with_limits(3, 100));

    let held: Vec<_> = (0..3)
        .map(|i| {
            slots
                .try_acquire("user-1")
                .unwrap_or_else(|_| panic!("stream {i} is within the cap and should be admitted"))
        })
        .collect();
    assert_eq!(slots.active_for_user("user-1"), 3);

    assert_eq!(
        slots.try_acquire("user-1").err(),
        Some(StreamRefusal::PerUser),
        "the stream past the cap must be refused, not queued"
    );
    drop(held);
}

/// A disconnect frees the slot. This is the property the whole guard design is
/// for: without it the cap would count lifetime opens and lock a family out after
/// three reconnects.
#[test]
fn test_closing_a_stream_frees_a_slot() {
    let slots = Arc::new(StreamSlots::with_limits(1, 100));

    let first = slots.try_acquire("user-1").expect("first stream fits");
    assert!(slots.try_acquire("user-1").is_err());

    drop(first);
    assert_eq!(slots.active_for_user("user-1"), 0);
    assert_eq!(slots.active_total(), 0, "the global count is released too");

    slots
        .try_acquire("user-1")
        .expect("a slot freed by a disconnect should be reusable");
}

/// The cap is per account, so one account hitting its limit must not cost a
/// neighbour anything.
#[test]
fn test_per_user_cap_does_not_leak_across_users() {
    let slots = Arc::new(StreamSlots::with_limits(1, 100));

    let _first = slots.try_acquire("user-1").expect("first account's stream");
    assert!(slots.try_acquire("user-1").is_err());

    let _second = slots
        .try_acquire("user-2")
        .expect("a different account must be unaffected by user-1's cap");
    assert_eq!(slots.active_for_user("user-2"), 1);
    assert_eq!(slots.active_total(), 2);
}

/// Many accounts, each politely under its own cap, still cannot exhaust the
/// replica's Redis connections.
#[test]
fn test_global_cap_bounds_a_distributed_open() {
    let slots = Arc::new(StreamSlots::with_limits(3, 2));

    let _a = slots.try_acquire("user-1").expect("first stream");
    let _b = slots.try_acquire("user-2").expect("second stream");

    assert_eq!(
        slots.try_acquire("user-3").err(),
        Some(StreamRefusal::Global),
        "a third account must hit the process cap, not its own"
    );
    assert_eq!(
        slots.try_acquire("user-1").err(),
        Some(StreamRefusal::Global),
        "the process cap outranks a per-user allowance that still has room"
    );
}

fn stream_claims() -> Claims {
    Claims {
        sub: "user-1".to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10_000_000_000,
    }
}

/// Opens a stream through the real handler and reports the status the client
/// would see. Cloud scope so no device lookup is needed on the admitted path.
async fn stream_status(state: AppState) -> StatusCode {
    match sync_stream_handler(
        State(state),
        axum::Extension(stream_claims()),
        HeaderMap::new(),
        axum::extract::Query(StreamQuery {
            client_id: Some("client-1".to_string()),
            device_uuid: None,
            scope: Some("scribble_keep_cloud".to_string()),
        }),
    )
    .await
    {
        Ok(_) => StatusCode::OK,
        Err(err) => err.into_response().status(),
    }
}

/// An account already at its cap gets 429 — and gets it without a Redis
/// connection or a config query being spent on it.
#[sqlx::test]
async fn test_stream_handler_refuses_over_cap_with_429(pool: PgPool) {
    let mut state = setup_state(pool);
    state.stream_slots = Arc::new(StreamSlots::with_limits(1, 100));
    let _held = state
        .stream_slots
        .try_acquire("user-1")
        .expect("the account's one allowed stream");

    assert_eq!(stream_status(state).await, StatusCode::TOO_MANY_REQUESTS);
}

/// A full replica answers 503: the request is fine, this pod just has no room.
#[sqlx::test]
async fn test_stream_handler_refuses_at_global_cap_with_503(pool: PgPool) {
    let mut state = setup_state(pool);
    state.stream_slots = Arc::new(StreamSlots::with_limits(3, 1));
    let _held = state
        .stream_slots
        .try_acquire("someone-else")
        .expect("the replica's one slot, held by another account");

    assert_eq!(stream_status(state).await, StatusCode::SERVICE_UNAVAILABLE);
}

/// The two refusals must keep their own status codes: a 429 tells a client to
/// stop opening streams and a 503 tells it to retry, and neither is a 403.
#[test]
fn test_limit_errors_map_to_their_status_codes() {
    assert_eq!(
        AppError::TooManyRequests("x".to_string())
            .into_response()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        AppError::Overloaded("x".to_string())
            .into_response()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
}

// --- The shared Redis pub/sub connection ------------------------------------
//
// These drive `SyncFanout` through its test seam rather than a live Redis: the
// manager task is the only part that needs a server, CI has none, and the parts
// worth pinning — one subscription per channel however many streams want it, the
// unsubscribe when the last one leaves, and the ordering the SSE handler's snapshot
// race depends on — are all above it. One end-to-end test against a real Redis
// follows, and skips when there is none, matching the tests above.

use crate::routes::sync::fanout::testing::{stubbed_fanout, RedisOp};
use crate::routes::sync::publisher::publish_user_event;

const FANOUT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Two streams on one account cost **one** Redis subscription, not two — the whole
/// point of the shared connection. The second registration must not reach Redis at
/// all: the first stream's subscription already covers it.
#[tokio::test]
async fn test_two_streams_for_one_user_share_a_single_subscription() {
    let (fanout, ops) = stubbed_fanout();
    let channel = get_channel_name("user-1");

    let first = fanout.subscribe(&channel).await.expect("first listener");
    let second = fanout.subscribe(&channel).await.expect("second listener");

    assert_eq!(fanout.listener_count(&channel), 2);
    assert_eq!(
        fanout.subscribed_channels(),
        1,
        "both streams must share one channel entry"
    );
    assert_eq!(
        ops.ops(),
        vec![RedisOp::Subscribe(channel.clone())],
        "the second stream must not issue a second SUBSCRIBE"
    );

    drop((first, second));
}

/// One published event reaches every stream listening on the channel. Fan-out in
/// process has to deliver what a per-connection subscribe used to deliver per
/// socket.
#[tokio::test]
async fn test_one_event_reaches_every_listener_on_a_channel() {
    let (fanout, _ops) = stubbed_fanout();
    let channel = get_channel_name("user-1");

    let mut first = fanout.subscribe(&channel).await.expect("first listener");
    let mut second = fanout.subscribe(&channel).await.expect("second listener");

    let event = SyncSseEvent::Invalidate {
        entity: "config".to_string(),
        sender_client_id: None,
        device_uuid: None,
    };
    let payload = serde_json::to_string(&event).unwrap();
    fanout.deliver_for_test(&channel, &payload);

    for listener in [&mut first, &mut second] {
        let received = tokio::time::timeout(FANOUT_TIMEOUT, listener.next())
            .await
            .expect("a listener should not have to wait for an already-published event")
            .expect("the listener's stream should still be open");
        assert_eq!(received, payload);
    }
}

/// A stream that goes away releases its place, and the last one out takes the Redis
/// subscription with it. Without this the shared connection would accumulate
/// subscriptions for disconnected users — the same unbounded growth in a new place.
#[tokio::test]
async fn test_the_last_listener_to_leave_unsubscribes_the_channel() {
    let (fanout, ops) = stubbed_fanout();
    let channel = get_channel_name("user-1");

    let first = fanout.subscribe(&channel).await.expect("first listener");
    let second = fanout.subscribe(&channel).await.expect("second listener");

    drop(first);
    assert_eq!(
        fanout.listener_count(&channel),
        1,
        "one stream leaving must not unregister the other"
    );
    assert_eq!(
        ops.ops(),
        vec![RedisOp::Subscribe(channel.clone())],
        "a channel somebody is still listening to must stay subscribed"
    );

    drop(second);
    assert_eq!(fanout.listener_count(&channel), 0);
    assert_eq!(
        fanout.subscribed_channels(),
        0,
        "the channel entry must be removed, not left behind at zero listeners"
    );
    assert_eq!(
        ops.wait_for(2).await,
        vec![
            RedisOp::Subscribe(channel.clone()),
            RedisOp::Unsubscribe(channel),
        ]
    );
}

/// The race the handler's ordering exists for: an event published after the stream
/// registered but before its snapshot query returns must still be delivered, not
/// fall down the gap between the two reads.
#[tokio::test]
async fn test_event_published_between_registration_and_snapshot_is_still_delivered() {
    let (fanout, _ops) = stubbed_fanout();
    let channel = get_channel_name("user-1");

    // Step 1 of the handler: register.
    let mut listener = fanout.subscribe(&channel).await.expect("listener");

    // A write lands here — after the subscribe, before the snapshot.
    let payload = serde_json::to_string(&SyncSseEvent::DirectUpdate {
        entity: "config".to_string(),
        key: "theme".to_string(),
        value: json!("dark"),
        sender_client_id: Some("someone-else".to_string()),
        device_uuid: None,
        is_deleted: false,
    })
    .unwrap();
    fanout.deliver_for_test(&channel, &payload);

    // Step 2: the snapshot query, which takes a while and is not polling the
    // listener. The buffered event has to survive it.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let received = tokio::time::timeout(FANOUT_TIMEOUT, listener.next())
        .await
        .expect("the event published during the snapshot must not be lost")
        .expect("the listener's stream should still be open");
    assert_eq!(received, payload);
}

/// End to end over a real Redis: one shared connection, two streams for the same
/// account, one publish, both fed. Skipped when no Redis is reachable — the same
/// stance as the Pub/Sub test above.
#[sqlx::test]
async fn test_shared_connection_feeds_two_streams_end_to_end(pool: PgPool) {
    let state = setup_state(pool);
    if state
        .redis_client
        .get_multiplexed_async_connection()
        .await
        .is_err()
    {
        eprintln!("SKIPPING test_shared_connection_feeds_two_streams_end_to_end: no Redis");
        return;
    }

    // A channel of its own, so a sibling test's traffic cannot be mistaken for ours.
    let user_id = format!("user-{}", uuid::Uuid::new_v4());
    let channel = get_channel_name(&user_id);

    let mut first = state.sync_fanout.subscribe(&channel).await.expect("first");
    let mut second = state.sync_fanout.subscribe(&channel).await.expect("second");
    assert_eq!(
        state.sync_fanout.listener_count(&channel),
        2,
        "two streams, one shared subscription"
    );

    let event = SyncSseEvent::Invalidate {
        entity: "config".to_string(),
        sender_client_id: None,
        device_uuid: None,
    };
    publish_user_event(&state.redis_publisher, &user_id, &event)
        .await
        .expect("publish should reach Redis");

    for listener in [&mut first, &mut second] {
        let payload = tokio::time::timeout(FANOUT_TIMEOUT, listener.next())
            .await
            .expect("timed out waiting for the event on the shared connection")
            .expect("the listener's stream should still be open");
        assert_eq!(
            serde_json::from_str::<SyncSseEvent>(&payload).unwrap(),
            event
        );
    }

    // And the account leaves nothing behind when both streams end.
    drop((first, second));
    assert_eq!(state.sync_fanout.listener_count(&channel), 0);
}

/// The single shared connection is a single point of failure, so what a listener
/// sees when it drops is part of the contract: its stream **ends**, which ends the
/// SSE response and sends the client back to reconnect into a fresh snapshot. That
/// is the recovery the old per-stream connection got for free when its own socket
/// died, and it is why a listener is never left holding a connection that has gone
/// deaf. The registry is emptied with it, so nothing is left to leak.
#[tokio::test]
async fn test_losing_the_shared_connection_ends_every_listener() {
    let (fanout, _ops) = stubbed_fanout();
    let channel = get_channel_name("user-1");

    let mut listener = fanout.subscribe(&channel).await.expect("listener");
    fanout.drop_connection_for_test();

    let ended = tokio::time::timeout(FANOUT_TIMEOUT, listener.next())
        .await
        .expect("the listener should be woken by the drop, not left hanging");
    assert!(
        ended.is_none(),
        "a listener on a dead connection must end so the client reconnects"
    );
    assert_eq!(
        fanout.subscribed_channels(),
        0,
        "the registry must not keep entries for a connection that is gone"
    );

    // And the fan-out is usable again once the manager reconnects.
    let _reconnected = fanout
        .subscribe(&channel)
        .await
        .expect("a stream opened after the drop should register again");
    assert_eq!(fanout.listener_count(&channel), 1);
}
