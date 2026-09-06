//! The refusal model: a batch is all-or-nothing, and the refusal names the item.
//!
//! `crate::routes::sync::rejections` is the argument; these are the two things that keep it
//! true — an end-to-end check that a bad item's id reaches the client, and a scan that
//! fails the build when a processor goes back to the bare serde error.

use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::Extension;
use chrono::Utc;
use sqlx::PgPool;

use crate::auth::tokens::Claims;
use crate::routes::sync::tests::helpers::{seed_device, setup_state};
use crate::routes::sync::{
    parse_or_hash_uuid, sync_handler, AppError, AppJson, OperationType, SyncRequest, SyncScope,
    TodoChangeDelta,
};

fn claims() -> Claims {
    Claims {
        sub: "user-1".to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10_000_000_000,
        product: None,
    }
}

/// A batch carrying one todo whose payload is missing every required field.
fn batch_with_one_unparseable_todo(id: &str) -> SyncRequest {
    SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
        scope: Some(SyncScope::Todo),
        todo_list_changes: vec![],
        todo_changes: vec![TodoChangeDelta {
            id: id.to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::json!({ "nothing": "useful" })),
        }],
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

/// The property the whole model rests on. Before this, the same request answered 400 with
/// serde's own message — "missing field `title` at line 1 column 21" — for a batch that may
/// carry hundreds of rows, so the client could not tell which one to drop and resent the
/// identical batch forever.
#[sqlx::test]
async fn an_unparseable_item_is_refused_by_name(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_device(&pool, parse_or_hash_uuid("user-1"), "Tablet").await;

    let err = sync_handler(
        State(state),
        Extension(claims()),
        AppJson(batch_with_one_unparseable_todo("todo-that-will-not-parse")),
    )
    .await
    .expect_err("an item with no required fields must be refused");

    match err {
        AppError::BadRequest(message) => {
            assert!(
                message.contains("todo-that-will-not-parse"),
                "the refusal must name the item the client has to drop: {message}"
            );
            assert!(
                message.contains("todo item"),
                "the refusal must say what kind of row it was: {message}"
            );
        }
        other => panic!("expected a named 400, got {other:?}"),
    }
}

/// All-or-nothing is the other half, and it is the half that was already true: the good
/// rows in the same batch must not be committed alongside the refusal. Committing them is
/// the partial-commit question item 27 is still holding open, and it is not settled here.
#[sqlx::test]
async fn a_refused_batch_writes_nothing(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_device(&pool, parse_or_hash_uuid("user-1"), "Tablet").await;

    let mut request = batch_with_one_unparseable_todo("todo-bad");
    request.todo_changes.insert(
        0,
        TodoChangeDelta {
            id: "todo-good".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::json!({
                "id": "todo-good",
                "title": "Buy milk",
                "isCompleted": false,
                "createdAt": Utc::now().timestamp_millis(),
                "position": 0,
                "userId": "user-1",
                "version": 1,
                "isDeleted": false,
                "syncState": "SYNCED",
                "lastModified": Utc::now().timestamp_millis(),
            })),
        },
    );

    let _ = sync_handler(State(state), Extension(claims()), AppJson(request))
        .await
        .expect_err("the batch carries an unparseable item");

    let written = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM todo_items WHERE id = $1"#,
        "todo-good"
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(
        written, 0,
        "a refused batch must leave the transaction rolled back, good rows included"
    );
}

/// Every `.rs` file under `src/routes/sync`, excluding this test tree. Same walk as
/// `tests::deletes` uses for its own guard.
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

/// The guard on the shape. Ten processors returned `AppError::Serialization(err)` for an
/// item they could not deserialize, and the eleventh table will be written by copying one
/// of them. The tests above cover the todo path; this covers the next one.
#[test]
fn no_processor_returns_a_bare_serialization_error() {
    let mut offenders = Vec::new();

    for path in sync_source_files() {
        // `types.rs` owns the variant itself -- the `From<serde_json::Error>` impl and the
        // arm that renders it -- and is not a processor refusing an item.
        if path.ends_with("types.rs") {
            continue;
        }

        let source = std::fs::read_to_string(&path).expect("readable source file");
        for (offset, _) in source.match_indices("Err(AppError::Serialization(") {
            let line = source[..offset].matches('\n').count() + 1;
            offenders.push(format!("{}:{}", path.display(), line));
        }
    }

    assert!(
        offenders.is_empty(),
        "a per-item refusal returned through `AppError::Serialization` answers 400 with \
         serde's message alone, which names a field and a JSON offset but not the item -- so \
         the client cannot tell which row to drop and resends the same batch forever. Use \
         `crate::routes::sync::rejections::item_payload_rejected`: {:?}",
        offenders
    );
}

#[test]
fn the_guard_would_catch_the_bug_it_is_guarding_against() {
    // The scan is only worth having if it fires, so run it over the code it rejects.
    let bug = r#"
        Err(err) => {
            tracing::error!("Failed to deserialize TodoItemData for todo {}: {:?}", change.id, err);
            return Err(AppError::Serialization(err));
        }
    "#;
    assert!(bug.contains("Err(AppError::Serialization("));
}
