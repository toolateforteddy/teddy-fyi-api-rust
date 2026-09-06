//! A delete for a row the server has never seen must not fail the batch.
//!
//! The policy and the reasoning live in `crate::routes::sync::deletes`. These tests
//! hold the line on both halves of it: the behaviour, through the real handler, and
//! the shape of the call sites, which is what drifted in the first place.

use crate::routes::sync::tests::helpers::{request, seed_device, setup_state, sync_handler};
use crate::routes::sync::{
    AppError, AppJson, CategoryChangeDelta, ConfigChangeDelta, DrawingChangeDelta,
    GroceryChangeDelta, GroceryItemStoreInfoChangeDelta, GroceryListChangeDelta,
    GroceryListMemberChangeDelta, OperationType, StoreChangeDelta, SyncRequest, SyncScope,
    TodoChangeDelta, TodoItemData, TodoListChangeDelta, parse_or_hash_uuid,
};
use axum::extract::State;
use sqlx::PgPool;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// A batch of deletes for ids that exist nowhere, one per table, alongside one
/// ordinary insert. Shaped like what a client sends when its "was this row ever
/// synced?" check is wrong: every pending delete it holds, in one request.
fn unsynced_delete_batch(device_uuid: Uuid, live_todo: &TodoItemData) -> SyncRequest {
    SyncRequest {
        device_uuid: Some(device_uuid),
        todo_list_changes: vec![TodoListChangeDelta {
            id: "never-synced-todo-list".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        todo_changes: vec![
            TodoChangeDelta {
                id: "never-synced-todo".to_string(),
                operation_type: OperationType::Delete,
                version: 3,
                data: None,
            },
            // The bystander: an ordinary write in the same batch, which the old
            // behaviour rolled back along with everything else.
            TodoChangeDelta {
                id: live_todo.id.clone(),
                operation_type: OperationType::Insert,
                version: 1,
                data: Some(serde_json::to_value(live_todo).unwrap()),
            },
        ],
        grocery_list_changes: vec![GroceryListChangeDelta {
            id: "never-synced-grocery-list".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        grocery_list_member_changes: vec![GroceryListMemberChangeDelta {
            id: "never-synced-membership".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        store_changes: vec![StoreChangeDelta {
            id: "never-synced-store".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        category_changes: vec![CategoryChangeDelta {
            id: "never-synced-category".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        grocery_changes: vec![GroceryChangeDelta {
            id: "never-synced-grocery-item".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        grocery_item_store_info_changes: vec![GroceryItemStoreInfoChangeDelta {
            id: "never-synced-store-info".to_string(),
            grocery_item_id: "never-synced-grocery-item".to_string(),
            store_id: "never-synced-store".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        ..request("client-1")
    }
}

/// The same thing for the scribble half of the API, which the handler only reaches
/// under the tablet scopes and so cannot ride along in the batch above.
fn unsynced_scribble_delete_batch(device_uuid: Uuid, config_id: Uuid, drawing_id: Uuid) -> SyncRequest {
    SyncRequest {
        device_uuid: Some(device_uuid),
        scope: Some(SyncScope::ScribbleKeep),
        config_changes: vec![ConfigChangeDelta {
            id: config_id.to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            device_uuid: Some(device_uuid),
            data: None,
        }],
        drawing_changes: vec![DrawingChangeDelta {
            id: drawing_id.to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            device_uuid: Some(device_uuid),
            data: None,
        }],
        ..request("client-1")
    }
}

fn live_todo_item() -> TodoItemData {
    TodoItemData {
        id: "todo-that-must-survive".to_string(),
        title: "Buy milk".to_string(),
        is_completed: false,
        created_at: 1000,
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
        // Set, so the icon path does not try to reach Gemini during the test.
        icon: Some("🥛".to_string()),
        sync_state: "SYNCED".to_string(),
        version: 1,
        is_deleted: false,
    }
}

#[sqlx::test]
async fn unsynced_deletes_are_acknowledged_and_do_not_fail_the_batch(pool: PgPool) {
    let state = setup_state(pool.clone());
    let device_uuid = seed_device(&pool, parse_or_hash_uuid("user-1"), "Tablet").await;
    let live_todo = live_todo_item();
    let req = unsynced_delete_batch(device_uuid, &live_todo);

    let deleted_ids: Vec<String> = [
        req.todo_list_changes[0].id.clone(),
        req.todo_changes[0].id.clone(),
        req.grocery_list_changes[0].id.clone(),
        req.grocery_list_member_changes[0].id.clone(),
        req.store_changes[0].id.clone(),
        req.category_changes[0].id.clone(),
        req.grocery_changes[0].id.clone(),
        req.grocery_item_store_info_changes[0].id.clone(),
    ]
    .to_vec();

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("a batch of deletes for rows the server never had must succeed")
        .0;

    // Every one of them acknowledged. An id the response does not mention stays
    // pending on the device and is resent on the next sync, forever.
    for id in &deleted_ids {
        assert!(
            res.success_ids.contains(id),
            "delete of unknown id {} was not acknowledged; success_ids: {:?}",
            id,
            res.success_ids
        );
        assert!(
            res.upload_status.iter().any(|s| &s.id == id && s.sync_state == "SYNCED"),
            "delete of unknown id {} missing from upload_status: {:?}",
            id,
            res.upload_status
        );
    }

    // And the unrelated write in the same batch committed: the failure being fixed
    // took the whole transaction down, not just the offending change.
    let survivor = sqlx::query!(
        "SELECT title FROM todo_items WHERE id = $1",
        live_todo.id
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert_eq!(
        survivor.map(|r| r.title),
        Some("Buy milk".to_string()),
        "the ordinary write in the batch was rolled back"
    );
}

#[sqlx::test]
async fn unsynced_scribble_deletes_are_acknowledged(pool: PgPool) {
    let state = setup_state(pool.clone());
    let device_uuid = seed_device(&pool, parse_or_hash_uuid("user-1"), "Tablet").await;
    let (config_id, drawing_id) = (Uuid::new_v4(), Uuid::new_v4());

    let res = sync_handler(
        State(state),
        AppJson(unsynced_scribble_delete_batch(device_uuid, config_id, drawing_id)),
    )
    .await
    .expect("deletes for a config and a drawing the server never had must succeed")
    .0;

    // These two never 500'd -- they simply said nothing about the change, which leaves
    // it pending on the tablet and resent on every sync from here on.
    for id in [config_id.to_string(), drawing_id.to_string()] {
        assert!(
            res.success_ids.contains(&id),
            "delete of unknown id {} was not acknowledged; success_ids: {:?}",
            id,
            res.success_ids
        );
    }
}

#[sqlx::test]
async fn unsynced_deletes_stay_idempotent_when_the_client_resends_them(pool: PgPool) {
    let state = setup_state(pool.clone());
    let device_uuid = seed_device(&pool, parse_or_hash_uuid("user-1"), "Tablet").await;
    let live_todo = live_todo_item();

    for attempt in 1..=2 {
        let req = unsynced_delete_batch(device_uuid, &live_todo);
        let res = sync_handler(State(state.clone()), AppJson(req))
            .await
            .unwrap_or_else(|e| panic!("attempt {} failed: {:?}", attempt, e))
            .0;
        assert!(
            res.success_ids.contains(&"never-synced-todo".to_string()),
            "attempt {} did not acknowledge the delete",
            attempt
        );
    }

    // No row was conjured for an id the account never had.
    let conjured = sqlx::query!(
        "SELECT count(*) as \"count!\" FROM todo_items WHERE id = $1",
        "never-synced-todo"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(conjured.count, 0, "an unsynced delete wrote a placeholder row");
}

#[sqlx::test]
async fn a_delete_for_someone_elses_row_is_still_refused(pool: PgPool) {
    // The row exists and belongs to another account: that is an authorization
    // failure, and acknowledging unknown ids must not have blurred it into one.
    sqlx::query!(
        r#"INSERT INTO todo_items (id, title, "isCompleted", "createdAt", position, "scheduledAt", "userId", "isDaily", priority, sync_state, version, updated_by_client, is_deleted)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"#,
        "someone-elses-todo", "Not yours", false, 0_i64, 0_i32, 0_i64, "user-2", false, 0_i32, "SYNCED", 1_i32, "client-9", false
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = setup_state(pool.clone());
    let live_todo = live_todo_item();
    let device_uuid = seed_device(&pool, parse_or_hash_uuid("user-1"), "Tablet").await;
    let mut req = unsynced_delete_batch(device_uuid, &live_todo);
    req.todo_changes[0].id = "someone-elses-todo".to_string();

    let err = sync_handler(State(state), AppJson(req))
        .await
        .expect_err("deleting another account's row must not be acknowledged");
    assert!(matches!(err, AppError::Forbidden(_)), "got {:?}", err);

    let untouched = sqlx::query!(
        "SELECT is_deleted FROM todo_items WHERE id = $1",
        "someone-elses-todo"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!untouched.is_deleted, "another account's row was soft-deleted");
}

/// Every `.rs` file under `src/routes/sync`, excluding this test tree.
fn sync_source_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("sync source directory") {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "tests") {
                    continue;
                }
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/routes/sync");
    let mut files = vec![root.with_extension("rs")];
    walk(&root, &mut files);
    files
}

/// The guard on the shape, not the behaviour: `fetch_one` on a soft-delete is the
/// exact construct that produced the 500, and it reappears the moment somebody
/// copies a delete arm into an eighth table. The tests above only cover the tables
/// that exist today; this covers the next one.
#[test]
fn no_soft_delete_uses_fetch_one() {
    let mut offenders = Vec::new();

    for path in sync_source_files() {
        let source = std::fs::read_to_string(&path).expect("readable source file");
        for (offset, _) in source.match_indices("is_deleted = TRUE") {
            // The statement being written, up to the end of the expression it is part
            // of -- far enough to reach the executor call, not so far as to reach the
            // next statement.
            let tail = &source[offset..];
            let statement = &tail[..tail.find(";\n").map(|i| i + 1).unwrap_or(tail.len())];
            if statement.contains(".fetch_one(") {
                let line = source[..offset].matches('\n').count() + 1;
                offenders.push(format!("{}:{}", path.display(), line));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "soft-delete run through `fetch_one`, which 500s the whole sync batch when the row \
         does not exist -- match the rows with `fetch_optional`/`fetch_all` and \
         acknowledge the ones that were not there (see `crate::routes::sync::deletes`): {:?}",
        offenders
    );
}

#[test]
fn the_guard_would_catch_the_bug_it_is_guarding_against() {
    // The scan above is only worth having if it actually fires, so run it over the
    // code it is meant to reject.
    let bug = r#"
        let row = sqlx::query!(
            "UPDATE todo_items SET is_deleted = TRUE, version = version + 1 WHERE id = $1 RETURNING version",
            change.id
        )
        .fetch_one(&mut **tx)
        .await?;
    "#;
    let offset = bug.find("is_deleted = TRUE").unwrap();
    let tail = &bug[offset..];
    let statement = &tail[..tail.find(";\n").map(|i| i + 1).unwrap_or(tail.len())];
    assert!(statement.contains(".fetch_one("));
}
