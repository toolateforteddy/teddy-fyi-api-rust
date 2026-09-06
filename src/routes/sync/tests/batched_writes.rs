//! What a payload that names the same row twice does.
//!
//! The processors issue one statement per *run* of same-kind writes rather than one per
//! change (`crate::routes::sync::batching`), and a set-based
//! `INSERT ... ON CONFLICT DO UPDATE` refuses a command whose input contains the same key
//! twice ("cannot affect row a second time"). A repeated id therefore has to end the run
//! it lands in, and these tests pin down that it does — a duplicate must behave exactly
//! as it did when every change was its own statement: the later write wins, and two
//! deletes of one row still move it two versions.

use crate::routes::sync::tests::helpers::{setup_state, sync_handler};
use crate::routes::sync::{
    AppJson, GroceryChangeDelta, GroceryItemData, OperationType, SyncRequest, TodoChangeDelta,
    TodoItemData,
};
use axum::extract::State;
use sqlx::PgPool;

fn blank_request(client_id: &str) -> SyncRequest {
    SyncRequest {
        last_synced_at: None,
        client_id: client_id.to_string(),
        device_uuid: None,
        device_name: None,
        scope: None,
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
        configs: vec![],
        drawings: vec![],
        supports_paging: false,
    }
}

fn grocery_item(id: &str, name: &str, version: i32) -> GroceryItemData {
    GroceryItemData {
        id: id.to_string(),
        name: name.to_string(),
        quantity: "1".to_string(),
        is_bought: false,
        created_at: 1_000,
        position: 0,
        category_id: None,
        times_bought: 0,
        user_id: Some("user-1".to_string()),
        is_active: true,
        list_id: None,
        unit: None,
        notes: None,
        sync_state: "PENDING_CREATE".to_string(),
        version,
        is_deleted: false,
    }
}

fn grocery_change(op: OperationType, data: &GroceryItemData) -> GroceryChangeDelta {
    GroceryChangeDelta {
        id: data.id.clone(),
        operation_type: op,
        version: data.version,
        data: Some(serde_json::to_value(data).unwrap()),
    }
}

fn todo_item(id: &str, title: &str, version: i32) -> TodoItemData {
    TodoItemData {
        id: id.to_string(),
        title: title.to_string(),
        is_completed: false,
        created_at: 1_000,
        position: 0,
        scheduled_date: None,
        recurrence_rule: None,
        scheduled_at: 0,
        user_id: Some("user-1".to_string()),
        parent_id: None,
        is_daily: false,
        due_date: None,
        description: None,
        list_id: None,
        priority: 0,
        // Non-empty so the icon assignment never reaches out to Gemini from a test.
        icon: Some("cart".to_string()),
        sync_state: "PENDING_CREATE".to_string(),
        version,
        is_deleted: false,
    }
}

fn todo_change(op: OperationType, data: &TodoItemData) -> TodoChangeDelta {
    TodoChangeDelta {
        id: data.id.clone(),
        operation_type: op,
        version: data.version,
        data: Some(serde_json::to_value(data).unwrap()),
    }
}

#[sqlx::test]
async fn grocery_item_written_twice_in_one_batch_keeps_the_later_write(pool: PgPool) {
    let state = setup_state(pool.clone());

    let mut req = blank_request("client-1");
    req.grocery_changes = vec![
        grocery_change(OperationType::Insert, &grocery_item("gi-1", "First", 0)),
        grocery_change(OperationType::Insert, &grocery_item("gi-1", "Second", 0)),
        grocery_change(OperationType::Insert, &grocery_item("gi-2", "Other", 0)),
    ];

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("a repeated id must not fail the batch")
        .0;

    // Every change is still acknowledged individually, in the order it arrived.
    assert_eq!(res.success_ids, vec!["gi-1", "gi-1", "gi-2"]);
    assert_eq!(
        res.upload_status
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        vec!["gi-1", "gi-1", "gi-2"]
    );

    let row = sqlx::query!("SELECT name FROM grocery_items WHERE id = 'gi-1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.name, "Second");

    let other = sqlx::query!("SELECT name FROM grocery_items WHERE id = 'gi-2'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(other.name, "Other");
}

#[sqlx::test]
async fn grocery_item_deleted_twice_in_one_batch_advances_twice(pool: PgPool) {
    let state = setup_state(pool.clone());

    let mut seed = blank_request("client-1");
    seed.grocery_changes = vec![grocery_change(
        OperationType::Insert,
        &grocery_item("gi-1", "First", 3),
    )];
    let _ = sync_handler(State(state.clone()), AppJson(seed))
        .await
        .expect("seed insert should succeed");

    let mut req = blank_request("client-1");
    req.grocery_changes = vec![
        GroceryChangeDelta {
            id: "gi-1".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        },
        GroceryChangeDelta {
            id: "gi-1".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        },
    ];

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("a repeated delete must not fail the batch")
        .0;

    // `version = version + 1` is decided by the row, so two deletes are two increments —
    // exactly what the per-item loop produced.
    let versions: Vec<i32> = res.upload_status.iter().map(|s| s.version).collect();
    assert_eq!(versions, vec![4, 5]);

    let row = sqlx::query!("SELECT version, is_deleted FROM grocery_items WHERE id = 'gi-1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.version, 5);
    assert!(row.is_deleted);
}

#[sqlx::test]
async fn grocery_item_deleted_then_reinserted_in_one_batch_is_present(pool: PgPool) {
    let state = setup_state(pool.clone());

    let mut seed = blank_request("client-1");
    seed.grocery_changes = vec![grocery_change(
        OperationType::Insert,
        &grocery_item("gi-1", "First", 0),
    )];
    let _ = sync_handler(State(state.clone()), AppJson(seed))
        .await
        .expect("seed insert should succeed");

    // The order the runs are issued in has to follow the order the changes arrived in:
    // hoisting all the upserts ahead of all the deletes would leave this row deleted.
    let mut req = blank_request("client-1");
    req.grocery_changes = vec![
        GroceryChangeDelta {
            id: "gi-1".to_string(),
            operation_type: OperationType::Delete,
            version: 0,
            data: None,
        },
        grocery_change(OperationType::Insert, &grocery_item("gi-1", "Back", 0)),
    ];
    let _ = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("delete followed by insert must not fail the batch");

    let row = sqlx::query!("SELECT name, is_deleted FROM grocery_items WHERE id = 'gi-1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.name, "Back");
    assert!(!row.is_deleted);
}

#[sqlx::test]
async fn todo_item_written_twice_in_one_batch_keeps_the_later_write(pool: PgPool) {
    let state = setup_state(pool.clone());

    let mut req = blank_request("client-1");
    req.todo_changes = vec![
        todo_change(OperationType::Insert, &todo_item("t-1", "First", 0)),
        todo_change(OperationType::Insert, &todo_item("t-1", "Second", 0)),
        todo_change(OperationType::Insert, &todo_item("t-2", "Other", 0)),
    ];

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("a repeated id must not fail the batch")
        .0;

    assert_eq!(res.success_ids, vec!["t-1", "t-1", "t-2"]);

    let row = sqlx::query!("SELECT title FROM todo_items WHERE id = 't-1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.title, "Second");

    let other = sqlx::query!("SELECT title FROM todo_items WHERE id = 't-2'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(other.title, "Other");
}

#[sqlx::test]
async fn todo_item_deleted_twice_in_one_batch_advances_twice(pool: PgPool) {
    let state = setup_state(pool.clone());

    let mut seed = blank_request("client-1");
    seed.todo_changes = vec![todo_change(
        OperationType::Insert,
        &todo_item("t-1", "First", 3),
    )];
    let _ = sync_handler(State(state.clone()), AppJson(seed))
        .await
        .expect("seed insert should succeed");

    let mut req = blank_request("client-1");
    req.todo_changes = vec![
        TodoChangeDelta {
            id: "t-1".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        },
        TodoChangeDelta {
            id: "t-1".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        },
    ];

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("a repeated delete must not fail the batch")
        .0;

    let versions: Vec<i32> = res.upload_status.iter().map(|s| s.version).collect();
    assert_eq!(versions, vec![4, 5]);

    let row = sqlx::query!("SELECT version, is_deleted FROM todo_items WHERE id = 't-1'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.version, 5);
    assert!(row.is_deleted);
}

#[sqlx::test]
async fn deletes_for_rows_the_server_never_had_are_still_acknowledged(pool: PgPool) {
    let state = setup_state(pool.clone());

    // Batched, a delete's version comes back from `RETURNING id, version`; ids that do not
    // come back are the rows that were never uploaded, and they are acknowledged rather
    // than failing the request. See `crate::routes::sync::deletes`.
    let mut req = blank_request("client-1");
    req.grocery_changes = vec![
        GroceryChangeDelta {
            id: "ghost-1".to_string(),
            operation_type: OperationType::Delete,
            version: 1,
            data: None,
        },
        GroceryChangeDelta {
            id: "ghost-2".to_string(),
            operation_type: OperationType::Delete,
            version: 1,
            data: None,
        },
    ];
    req.todo_changes = vec![TodoChangeDelta {
        id: "ghost-t".to_string(),
        operation_type: OperationType::Delete,
        version: 1,
        data: None,
    }];

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("deletes for unknown rows must be acknowledged")
        .0;

    assert_eq!(res.success_ids.len(), 3);
    for status in &res.upload_status {
        assert_eq!(status.version, 1, "{} should report the unsynced-delete version", status.id);
        assert_eq!(status.sync_state, "SYNCED");
    }
}

// --- Configs and drawings ---------------------------------------------------------
//
// These two tables batch their writes on the same run rule as the tables above, but they
// reach it differently: their processors also *read* the server through a per-batch cache
// (`ConfigBatch` / `DrawingBatch`) that marks a row stale once written and re-reads it for
// the next item that names it. A buffered write is not in the database yet, so that re-read
// would answer with the row as it was before it. The processors therefore flush the run
// before any such lookup — see `RunTracker::contains` — and the delete-then-reinsert tests
// below are what pin that down.
//
// The multi-row tests are the other half: everything else in this file uses two or three
// items, which a mis-zipped `UNNEST` column list can still satisfy by accident. Five rows
// with five distinct values cannot be.

use crate::routes::sync::tests::helpers::seed_device;
use crate::routes::sync::{ConfigSyncItem, DrawingSyncItem, SyncScope, parse_or_hash_uuid};
use chrono::Utc;

fn scribble_request(client_id: &str) -> SyncRequest {
    let mut req = blank_request(client_id);
    req.scope = Some(SyncScope::ScribbleKeep);
    req
}

fn drawing_item(id: uuid::Uuid, marker: &str, version: i32) -> DrawingSyncItem {
    DrawingSyncItem {
        id,
        user_id: None,
        device_uuid: None,
        created_at: 1_000,
        data: serde_json::json!({ "marker": marker }),
        sync_state: "PENDING_INSERT".to_string(),
        version,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    }
}

fn config_item(id: uuid::Uuid, key: &str, value: &str) -> ConfigSyncItem {
    ConfigSyncItem {
        id,
        device_uuid: None,
        key: key.to_string(),
        value: value.to_string(),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    }
}

/// Five drawings in one statement, each with its own blob.
///
/// The batched insert zips seven parallel column vectors back into rows; a column pushed in
/// the wrong order, or one push missed on a branch, produces rows that are individually
/// well-formed but carry each other's values. Distinct markers are what catch that.
#[sqlx::test]
async fn many_drawings_in_one_batch_each_keep_their_own_blob(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;

    let ids: Vec<uuid::Uuid> = (0..5).map(|_| uuid::Uuid::new_v4()).collect();
    let mut req = scribble_request("client-1");
    req.drawings = ids
        .iter()
        .enumerate()
        .map(|(i, id)| drawing_item(*id, &format!("marker-{i}"), 1))
        .collect();

    let _ = sync_handler(State(state), AppJson(req))
        .await
        .expect("a five-drawing batch must not fail");

    for (i, id) in ids.iter().enumerate() {
        let row = sqlx::query!("SELECT data, version FROM drawings WHERE id = $1", id)
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|_| panic!("drawing {i} should have been written"));
        assert_eq!(
            row.data,
            serde_json::json!({ "marker": format!("marker-{i}") }),
            "drawing {i} carries another row's blob"
        );
        assert_eq!(row.version, 1, "drawing {i} has the wrong version");
    }
}

/// The same, for configs: five keys, five values, one statement.
#[sqlx::test]
async fn many_configs_in_one_batch_each_keep_their_own_value(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;

    let mut req = scribble_request("client-1");
    req.configs = (0..5)
        .map(|i| config_item(uuid::Uuid::new_v4(), &format!("key-{i}"), &format!("value-{i}")))
        .collect();

    let _ = sync_handler(State(state), AppJson(req))
        .await
        .expect("a five-config batch must not fail");

    for i in 0..5 {
        let row = sqlx::query!(
            "SELECT value FROM configs WHERE user_id = $1 AND key = $2",
            user_uuid,
            format!("key-{i}")
        )
        .fetch_one(&pool)
        .await
        .unwrap_or_else(|_| panic!("config key-{i} should have been written"));
        assert_eq!(row.value, format!("value-{i}"), "config {i} carries another row's value");
    }
}

/// A drawing deleted and then re-uploaded inside one payload is present at the end.
///
/// The delete and the upsert are different write kinds, so they cannot share a run; if the
/// upserts were hoisted ahead of the deletes the row would end up deleted instead. The
/// re-upload also has to see the version the delete assigned, which is only true if the
/// delete reached the database before the upsert resolved it.
#[sqlx::test]
async fn drawing_deleted_then_reuploaded_in_one_batch_is_present(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;

    let id = uuid::Uuid::new_v4();

    let mut seed = scribble_request("client-1");
    seed.drawings = vec![drawing_item(id, "original", 1)];
    let _ = sync_handler(State(state.clone()), AppJson(seed))
        .await
        .expect("seeding the drawing must succeed");

    let mut req = scribble_request("client-1");
    let mut deleted = drawing_item(id, "original", 2);
    deleted.is_deleted = true;
    deleted.sync_state = "PENDING_DELETE".to_string();
    req.drawings = vec![deleted, drawing_item(id, "revived", 3)];

    let _ = sync_handler(State(state), AppJson(req))
        .await
        .expect("a delete-then-reupload payload must not fail");

    let row = sqlx::query!("SELECT is_deleted, data, version FROM drawings WHERE id = $1", id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!row.is_deleted, "the re-upload must win over the delete");
    assert_eq!(row.data, serde_json::json!({ "marker": "revived" }));
    // Seed wrote v1, the delete advanced it to v2, the re-upload to v3. The re-upload
    // numbering off v2 is what proves the delete had landed before it resolved.
    assert_eq!(row.version, 3, "the re-upload did not see the delete's version");
}

/// The same shape for configs, where the row is addressed by `(device, key)` as well as by
/// id — so the flush before the re-read has to cover the key, not only the id.
#[sqlx::test]
async fn config_deleted_then_rewritten_in_one_batch_is_present(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;

    let id = uuid::Uuid::new_v4();

    let mut seed = scribble_request("client-1");
    seed.configs = vec![config_item(id, "theme", "dark")];
    let _ = sync_handler(State(state.clone()), AppJson(seed))
        .await
        .expect("seeding the config must succeed");

    let mut req = scribble_request("client-1");
    let mut deleted = config_item(id, "theme", "dark");
    deleted.is_deleted = true;
    deleted.sync_state = "PENDING_DELETE".to_string();
    req.configs = vec![deleted, config_item(id, "theme", "light")];

    let _ = sync_handler(State(state), AppJson(req))
        .await
        .expect("a delete-then-rewrite payload must not fail");

    let rows = sqlx::query!(
        "SELECT value, is_deleted, version FROM configs WHERE user_id = $1 AND key = 'theme'",
        user_uuid
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "the key must still hold exactly one row");
    assert_eq!(rows[0].value, "light", "the rewrite must win over the delete");
    assert!(!rows[0].is_deleted);
    assert_eq!(rows[0].version, 3, "the rewrite did not see the delete's version");
}
