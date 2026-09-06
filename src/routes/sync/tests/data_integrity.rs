//! What the sync path refuses to take from the client.
//!
//! Three things used to be decided by numbers in the request body: which of two competing
//! writes won (the client's `last_modified`), what version a row moved to (the client's
//! `version`, through `max(server, client) + 1`), and how large a drawing or a config
//! could be (nothing at all). These tests pin the replacements down — see
//! `crate::routes::sync::versioning` for the conflict policy and
//! `crate::routes::sync::limits` for the bounds.

use crate::routes::sync::tests::helpers::{request, seed_device, setup_state, sync_handler};
use crate::routes::sync::versioning::{MAX_SEED_VERSION, MAX_SYNC_VERSION};
use crate::routes::sync::limits::{DEFAULT_MAX_ITEMS_PER_COLLECTION, DEFAULT_MAX_ITEMS_TOTAL};
use crate::routes::sync::{
    AppError, AppJson, ConfigChangeDelta, DrawingSyncItem, GroceryListChangeDelta, GroceryListData,
    OperationType, SyncRequest, SyncScope, ConfigSyncItem, parse_or_hash_uuid,
};
use axum::extract::State;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

/// An otherwise empty sync body, so each test shows only the field it is about.
fn blank_request(client_id: &str, scope: SyncScope) -> SyncRequest {
    SyncRequest {
        scope: Some(scope),
        ..request(&client_id)
    }
}

fn drawing(id: Uuid, version: i32, last_modified: i64, data: serde_json::Value) -> DrawingSyncItem {
    DrawingSyncItem {
        id,
        user_id: None,
        device_uuid: None,
        created_at: 1_000,
        data,
        sync_state: "PENDING_UPDATE".to_string(),
        version,
        is_deleted: false,
        last_modified,
    }
}

/// A stamp ten years out — the shape a broken clock or a hostile client produces.
fn far_future_ms() -> i64 {
    (Utc::now() + chrono::Duration::days(3_650)).timestamp_millis()
}

/// The stamp a client claims is not the stamp that gets stored, and cannot be used to
/// lock a row against everybody else.
///
/// Under the old rule this was the whole attack: one write with `last_modified` far in the
/// future made every later write lose the `client last_modified >= server last_modified`
/// comparison, permanently.
#[sqlx::test]
async fn a_future_dated_stamp_is_not_stored_and_does_not_win_the_next_conflict(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;
    let drawing_id = Uuid::new_v4();
    let claimed = far_future_ms();

    let mut req = blank_request("client-1", SyncScope::ScribbleKeep);
    req.drawings = vec![drawing(drawing_id, 1, claimed, serde_json::json!({"strokes": ["a"]}))];
    let _ = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("the write itself is fine; only the stamp is not taken on trust");

    let row = sqlx::query!(
        "SELECT last_modified, client_last_modified, version FROM drawings WHERE id = $1",
        drawing_id
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    let now = Utc::now().timestamp_millis();
    assert!(
        (row.last_modified - now).abs() < 60_000,
        "last_modified should be the server's clock, got {} against now {}",
        row.last_modified,
        now
    );
    assert_eq!(
        row.client_last_modified,
        Some(claimed),
        "the client's claim is kept, just not made authoritative"
    );

    // A second device, with an honest clock and a stale version, writes over it. Under
    // the old rule its `now` stamp lost to the stored ten-years-hence one and the update
    // was dropped; the row was unwritable by anyone else for a decade.
    let mut second = blank_request("client-2", SyncScope::ScribbleKeep);
    second.drawings = vec![drawing(
        drawing_id,
        0,
        Utc::now().timestamp_millis(),
        serde_json::json!({"strokes": ["b"]}),
    )];
    let _ = sync_handler(State(state), AppJson(second))
        .await
        .expect("Handler should succeed");

    let row = sqlx::query!(
        "SELECT version, data FROM drawings WHERE id = $1",
        drawing_id
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.data, serde_json::json!({"strokes": ["b"]}));
    assert_eq!(row.version, 2, "server row's version + 1, not the client's");
}

/// A tablet that was edited offline still gets its edit in when it reconnects, even though
/// the row moved on without it and its own stamp is older than the server's.
#[sqlx::test]
async fn a_legitimate_offline_edit_still_lands(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;
    let drawing_id = Uuid::new_v4();

    // The server-side row: five versions in, written a moment ago by another device.
    sqlx::query!(
        "INSERT INTO drawings (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data) \
         VALUES ($1, $2, $3, $4, 5, false, $5, 'SYNCED'::sync_state, 1000, $6)",
        drawing_id,
        user_uuid,
        device_uuid,
        parse_or_hash_uuid("client-2"),
        Utc::now().timestamp_millis(),
        serde_json::json!({"strokes": ["server"]})
    )
    .execute(&pool)
    .await
    .unwrap();

    // The offline tablet: based on version 2, edited an hour ago.
    let mut req = blank_request("client-1", SyncScope::ScribbleKeep);
    req.drawings = vec![drawing(
        drawing_id,
        2,
        (Utc::now() - chrono::Duration::hours(1)).timestamp_millis(),
        serde_json::json!({"strokes": ["offline"]}),
    )];
    let _ = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed");

    let row = sqlx::query!("SELECT version, data FROM drawings WHERE id = $1", drawing_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        row.data,
        serde_json::json!({"strokes": ["offline"]}),
        "the reconnecting device's work must not be silently dropped"
    );
    assert_eq!(row.version, 6);
}

/// An enormous `version` on an existing row is ignored rather than adopted.
///
/// This is the grocery half of the story: the counter used to be
/// `max(server, client) + 1`, so one request could move a shared list's version to two
/// billion and leave it there.
#[sqlx::test]
async fn an_inflated_version_cannot_jump_the_counter(pool: PgPool) {
    let state = setup_state(pool.clone());
    let list_id = "glist-inflate";

    let insert_data = GroceryListData {
        id: list_id.to_string(),
        name: "Weekly shop".to_string(),
        owner_id: Some("user-1".to_string()),
        created_at: 123,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };
    let mut req = blank_request("client-1", SyncScope::Grocery);
    req.grocery_list_changes = vec![GroceryListChangeDelta {
        id: list_id.to_string(),
        operation_type: OperationType::Insert,
        version: 1,
        data: Some(serde_json::to_value(&insert_data).unwrap()),
    }];
    let _ = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("Handler should succeed");

    let mut inflated = insert_data.clone();
    inflated.version = MAX_SYNC_VERSION;
    let mut req = blank_request("client-1", SyncScope::Grocery);
    req.grocery_list_changes = vec![GroceryListChangeDelta {
        id: list_id.to_string(),
        operation_type: OperationType::Update,
        version: MAX_SYNC_VERSION,
        data: Some(serde_json::to_value(&inflated).unwrap()),
    }];
    let _ = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed");

    let row = sqlx::query!("SELECT version FROM grocery_lists WHERE id = $1", list_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        row.version, 2,
        "one past the server's own version, whatever the client claimed"
    );
}

/// A brand-new row is the one place a client's version is used at all, and it is bounded
/// there — so the ceiling cannot be reached by seeding a row next to it.
#[sqlx::test]
async fn an_inflated_seed_on_a_new_row_is_refused(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;
    let drawing_id = Uuid::new_v4();

    let mut req = blank_request("client-1", SyncScope::ScribbleKeep);
    req.drawings = vec![drawing(
        drawing_id,
        MAX_SEED_VERSION + 1,
        Utc::now().timestamp_millis(),
        serde_json::json!({"strokes": []}),
    )];
    let err = sync_handler(State(state), AppJson(req)).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("version")),
        "got {:?}",
        err
    );

    let count = sqlx::query!("SELECT count(*) FROM drawings WHERE id = $1", drawing_id)
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
    assert_eq!(count, 0, "nothing written");
}

/// At the ceiling the write is refused cleanly (409) rather than overflowing the `i32`.
#[sqlx::test]
async fn a_row_at_the_version_ceiling_refuses_with_a_conflict(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;
    let drawing_id = Uuid::new_v4();

    sqlx::query!(
        "INSERT INTO drawings (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data) \
         VALUES ($1, $2, $3, $4, $5, false, 1, 'SYNCED'::sync_state, 1000, $6)",
        drawing_id,
        user_uuid,
        device_uuid,
        parse_or_hash_uuid("client-2"),
        MAX_SYNC_VERSION,
        serde_json::json!({"strokes": []})
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut req = blank_request("client-1", SyncScope::ScribbleKeep);
    req.drawings = vec![drawing(
        drawing_id,
        1,
        Utc::now().timestamp_millis(),
        serde_json::json!({"strokes": ["new"]}),
    )];
    let err = sync_handler(State(state), AppJson(req)).await.unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)), "got {:?}", err);

    let row = sqlx::query!("SELECT version, data FROM drawings WHERE id = $1", drawing_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.version, MAX_SYNC_VERSION, "unchanged, not wrapped");
    assert_eq!(row.data, serde_json::json!({"strokes": []}));
}

/// An over-large drawing blob is refused, and the refusal names the field and the unit.
/// Nothing is written — a child's drawing is never silently truncated.
#[sqlx::test]
async fn an_oversized_drawing_is_refused_and_nothing_is_written(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;
    let drawing_id = Uuid::new_v4();

    let mut req = blank_request("client-1", SyncScope::ScribbleKeep);
    req.drawings = vec![drawing(
        drawing_id,
        1,
        Utc::now().timestamp_millis(),
        serde_json::json!({ "strokes": "x".repeat(600 * 1024) }),
    )];
    let err = sync_handler(State(state), AppJson(req)).await.unwrap_err();
    match err {
        AppError::BadRequest(msg) => {
            assert!(msg.contains("drawings[].data"), "{}", msg);
            assert!(msg.contains("bytes"), "{}", msg);
        }
        other => panic!("expected BadRequest, got {:?}", other),
    }

    let count = sqlx::query!("SELECT count(*) FROM drawings WHERE id = $1", drawing_id)
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
    assert_eq!(count, 0);
}

/// An over-long config key or value is refused the same way.
#[sqlx::test]
async fn an_oversized_config_is_refused_and_nothing_is_written(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;

    let long_value_id = Uuid::new_v4();
    let mut req = blank_request("client-1", SyncScope::ScribbleKeep);
    req.configs = vec![ConfigSyncItem {
        id: long_value_id,
        device_uuid: None,
        key: "child_name".to_string(),
        value: "v".repeat(16 * 1024),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    }];
    let err = sync_handler(State(state.clone()), AppJson(req)).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("value")),
        "got {:?}",
        err
    );

    let long_key_id = Uuid::new_v4();
    let mut req = blank_request("client-1", SyncScope::ScribbleKeep);
    req.configs = vec![ConfigSyncItem {
        id: long_key_id,
        device_uuid: None,
        key: "k".repeat(200),
        value: "dark".to_string(),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    }];
    let err = sync_handler(State(state), AppJson(req)).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("key")),
        "got {:?}",
        err
    );

    let count = sqlx::query!(
        "SELECT count(*) FROM configs WHERE id = ANY($1)",
        &[long_value_id, long_key_id][..]
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .count
    .unwrap();
    assert_eq!(count, 0);
}

/// The ordinary case is untouched: a normal drawing and a normal config both land, and the
/// row carries the server's stamp with the client's claim beside it.
#[sqlx::test]
async fn ordinary_payloads_are_unaffected(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;
    let drawing_id = Uuid::new_v4();
    let config_id = Uuid::new_v4();
    let claimed = Utc::now().timestamp_millis();

    let mut req = blank_request("client-1", SyncScope::ScribbleKeep);
    req.drawings = vec![drawing(
        drawing_id,
        1,
        claimed,
        serde_json::json!({"strokes": [{"points": [1, 2], "color": "#fff"}]}),
    )];
    req.configs = vec![ConfigSyncItem {
        id: config_id,
        device_uuid: None,
        key: "selected_theme".to_string(),
        value: "dark".to_string(),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: claimed,
    }];

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res.drawings.iter().any(|d| d.id == drawing_id));
    assert!(res.configs.iter().any(|c| c.id == config_id));

    let cfg = sqlx::query!(
        "SELECT version, key, value, client_last_modified FROM configs WHERE id = $1",
        config_id
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(cfg.version, 1, "a fresh row keeps the client's seed");
    assert_eq!(cfg.key, "selected_theme");
    assert_eq!(cfg.value, "dark");
    assert_eq!(cfg.client_last_modified, Some(claimed));
}


/// A config item that is legal in every way except, in bulk, how many of it there are.
fn config(index: usize) -> ConfigSyncItem {
    ConfigSyncItem {
        id: Uuid::new_v4(),
        device_uuid: None,
        key: format!("k{}", index),
        value: "v".to_string(),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: 1_000,
    }
}

fn configs(count: usize) -> Vec<ConfigSyncItem> {
    (0..count).map(config).collect()
}

/// A body that is small in bytes and enormous in statements is refused, and nothing is
/// written.
///
/// This is the shape the per-item size bounds do not see: every item here is a handful of
/// bytes and perfectly well formed, so each one passes every check in
/// `crate::routes::sync::limits` except the count. Processed, they would be one sequential
/// `INSERT` each inside a single transaction.
#[sqlx::test]
async fn an_over_count_collection_is_refused_and_nothing_is_written(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;

    let mut req = blank_request("client-1", SyncScope::ScribbleKeep);
    req.configs = configs(DEFAULT_MAX_ITEMS_PER_COLLECTION + 1);

    let err = sync_handler(State(state), AppJson(req)).await.unwrap_err();
    match err {
        AppError::BadRequest(msg) => {
            assert!(msg.contains("configs"), "{}", msg);
            assert!(msg.contains("items"), "{}", msg);
            assert!(
                msg.contains(&DEFAULT_MAX_ITEMS_PER_COLLECTION.to_string()),
                "the refusal names the limit: {}",
                msg
            );
        }
        other => panic!("expected BadRequest, got {:?}", other),
    }

    let count = sqlx::query!("SELECT count(*) FROM configs")
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
    assert_eq!(count, 0, "the refusal happens before the transaction opens");
}

/// The gap a per-collection cap alone leaves: twelve vectors, each legal on its own,
/// adding up to the very request the cap was meant to refuse.
#[sqlx::test]
async fn collections_within_their_own_cap_still_bust_the_request_total(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;

    let mut req = blank_request("client-1", SyncScope::ScribbleKeep);
    req.configs = configs(DEFAULT_MAX_ITEMS_PER_COLLECTION);
    req.config_changes = (0..DEFAULT_MAX_ITEMS_PER_COLLECTION)
        .map(|i| ConfigChangeDelta {
            id: format!("c{}", i),
            operation_type: OperationType::Delete,
            version: 1,
            device_uuid: None,
            data: None,
        })
        .collect();
    // Two full-but-legal collections are already the whole total; one more item of
    // anything is what tips it over.
    req.drawings = vec![drawing(Uuid::new_v4(), 1, 1_000, serde_json::json!({"strokes": []}))];
    for len in [req.configs.len(), req.config_changes.len(), req.drawings.len()] {
        assert!(len <= DEFAULT_MAX_ITEMS_PER_COLLECTION);
    }
    assert!(
        req.configs.len() + req.config_changes.len() + req.drawings.len() > DEFAULT_MAX_ITEMS_TOTAL
    );

    let err = sync_handler(State(state), AppJson(req)).await.unwrap_err();
    assert!(
        matches!(err, AppError::BadRequest(ref m) if m.contains("across all change collections")),
        "got {:?}",
        err
    );

    let count = sqlx::query!("SELECT count(*) FROM configs")
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
    assert_eq!(count, 0);
}

/// The other half of the bargain: a large, entirely legitimate batch still lands.
///
/// Sized above what the shipped client can produce in one request — `RoomSyncEngine`
/// batches at 25 drawings, and sends its configs unbatched at roughly thirty keys per
/// device — so a first sync from a tablet that has been offline for months goes through
/// untouched.
#[sqlx::test]
async fn a_large_but_legitimate_batch_still_succeeds(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;

    let mut req = blank_request("client-1", SyncScope::ScribbleKeep);
    req.configs = configs(300);
    req.drawings = (0..25)
        .map(|i| {
            drawing(
                Uuid::new_v4(),
                1,
                Utc::now().timestamp_millis(),
                serde_json::json!({ "strokes": [{ "points": [i, i], "color": "#fff" }] }),
            )
        })
        .collect();

    let _ = sync_handler(State(state), AppJson(req))
        .await
        .expect("a real client batch is not what the count limit is for");

    let config_count = sqlx::query!("SELECT count(*) FROM configs")
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
    assert_eq!(config_count, 300);

    let drawing_count = sqlx::query!("SELECT count(*) FROM drawings")
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
    assert_eq!(drawing_count, 25);
}
