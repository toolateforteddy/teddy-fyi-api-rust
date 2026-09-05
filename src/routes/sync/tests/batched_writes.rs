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
