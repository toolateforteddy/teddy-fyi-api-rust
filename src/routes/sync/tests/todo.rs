use sqlx::PgPool;
use axum::extract::State;
use chrono::Utc;
use crate::routes::sync::tests::helpers::{setup_state, sync_handler};
use crate::routes::sync::{
    SyncRequest, SyncScope, TodoListData, TodoItemData, TodoListChangeDelta, TodoChangeDelta, OperationType
};

#[sqlx::test]
async fn test_sync_handler_insert_todo_list(pool: PgPool) {
    let state = setup_state(pool.clone());
    let list_data = TodoListData {
        id: "list-1".to_string(),
        name: "Test List".to_string(),
        color_hex: "#FF0000".to_string(),
        user_id: Some("user-1".to_string()),
        created_at: 0,
        sync_state: "SYNCED".to_string(),
        version: 1,
        is_deleted: false,
    };
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
        scope: None,
        todo_list_changes: vec![TodoListChangeDelta {
            id: "list-1".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&list_data).unwrap()),
        }],
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
    };

    let res = sync_handler(State(state), crate::routes::sync::AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res.success_ids, vec!["list-1"]);
}

#[sqlx::test]
async fn test_sync_handler_insert_todo(pool: PgPool) {
    let state = setup_state(pool.clone());
    let todo_data = TodoItemData {
        id: "todo-1".to_string(),
        title: "Test Todo".to_string(),
        is_completed: false,
        created_at: 0,
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
        icon: None,
        sync_state: "SYNCED".to_string(),
        version: 1,
        is_deleted: false,
    };
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![TodoChangeDelta {
            id: "todo-1".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&todo_data).unwrap()),
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
    };

    let res = sync_handler(State(state), crate::routes::sync::AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res.success_ids, vec!["todo-1"]);
}

#[sqlx::test]
async fn test_sync_handler_update_todo(pool: PgPool) {
    sqlx::query!(
        "INSERT INTO todo_items (id, title, \"isCompleted\", \"createdAt\", position, \"scheduledAt\", \"userId\", \"isDaily\", priority, icon, sync_state, version, updated_by_client, is_deleted)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        "todo-2", "Test Todo", false, 0_i64, 0_i32, 0_i64, "user-1", false, 0_i32, None::<String>, "SYNCED", 1_i32, "client-1", false
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = setup_state(pool.clone());
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-2".to_string(),
        device_uuid: None,
        device_name: None,
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![TodoChangeDelta {
            id: "todo-2".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: None,
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
    };

    let res = sync_handler(State(state), crate::routes::sync::AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res.success_ids, vec!["todo-2"]);

    // The remote changes list must now contain the full serialized record with version 1
    assert_eq!(res.remote_todo_changes.len(), 1);
    let remote_todo = &res.remote_todo_changes[0];
    assert_eq!(remote_todo.id, "todo-2");
    assert_eq!(remote_todo.version, 1);
    let data_val: TodoItemData = serde_json::from_value(remote_todo.data.as_ref().unwrap().clone()).unwrap();
    assert_eq!(data_val.title, "Test Todo");
    assert_eq!(data_val.version, 1);

    // No-op on write: DB version and updated_by_client must not change
    let updated = sqlx::query!(
        "SELECT version, updated_by_client FROM todo_items WHERE id = $1",
        "todo-2"
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(updated.version, 1);
    assert_eq!(updated.updated_by_client, Some("client-1".to_string()));
}

#[sqlx::test]
async fn test_sync_handler_delete_todo(pool: PgPool) {
    sqlx::query!(
        "INSERT INTO todo_items (id, title, \"isCompleted\", \"createdAt\", position, \"scheduledAt\", \"userId\", \"isDaily\", priority, icon, sync_state, version, updated_by_client, is_deleted)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        "todo-3", "Test Todo", false, 0_i64, 0_i32, 0_i64, "user-1", false, 0_i32, None::<String>, "SYNCED", 1_i32, "client-1", false
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = setup_state(pool.clone());
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-2".to_string(),
        device_uuid: None,
        device_name: None,
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![TodoChangeDelta {
            id: "todo-3".to_string(),
            operation_type: OperationType::Delete,
            version: 2,
            data: None,
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
    };

    let res = sync_handler(State(state), crate::routes::sync::AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res.success_ids, vec!["todo-3"]);

    let updated = sqlx::query!(
        "SELECT is_deleted, updated_by_client FROM todo_items WHERE id = $1",
        "todo-3"
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(updated.is_deleted);
    assert_eq!(updated.updated_by_client, Some("client-2".to_string()));
}

#[sqlx::test]
async fn test_sync_handler_scope_todo(pool: PgPool) {
    let state = setup_state(pool.clone());
    let other_client = "other-client";
    
    // Todo List
    sqlx::query!(
        r#"INSERT INTO todo_lists (id, name, "colorHex", "userId", "createdAt", sync_state, version, is_deleted, updated_at, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), $9)"#,
        "todolist-scope-2",
        "Scope List",
        "#FF0000",
        "user-1",
        0_i64,
        "SYNCED",
        1_i32,
        false,
        other_client
    )
    .execute(&pool)
    .await
    .unwrap();

    // Grocery List
    sqlx::query!(
        r#"INSERT INTO grocery_lists (id, name, "ownerId", "createdAt", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, NOW(), $6, $7, $8)"#,
        "grocerylist-scope-2",
        "Scope Grocery List",
        "owner-1",
        0_i64,
        1_i32,
        other_client,
        false,
        "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
        scope: Some(SyncScope::Todo),
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
    };

    let res = sync_handler(State(state), crate::routes::sync::AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    assert!(res.remote_todo_list_changes.iter().any(|d| d.id == "todolist-scope-2"));
    assert!(res.remote_grocery_list_changes.is_empty());
}

/// A todo the server put an icon on is written with the icon and marked `SERVER-AI`.
///
/// The icon arrives as a pre-resolved map rather than being fetched here, which is the
/// point of the test: `process_todo_changes` is handed no `AppState`, no HTTP client and
/// no API key, so it *cannot* make an outbound call while this transaction is open. The
/// Gemini round trip now happens in `resolve_todo_icons`, before `begin()`.
#[sqlx::test]
async fn test_process_todo_changes_applies_preresolved_icon(pool: PgPool) {
    let todo_data = TodoItemData {
        id: "todo-icon-1".to_string(),
        title: "Fix the kitchen tap".to_string(),
        is_completed: false,
        created_at: 0,
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
        icon: None,
        sync_state: "SYNCED".to_string(),
        version: 1,
        is_deleted: false,
    };
    let changes = vec![TodoChangeDelta {
        id: "todo-icon-1".to_string(),
        operation_type: OperationType::Insert,
        version: 1,
        data: Some(serde_json::to_value(&todo_data).unwrap()),
    }];

    let mut resolved_icons = std::collections::HashMap::new();
    resolved_icons.insert("todo-icon-1".to_string(), "Plumbing".to_string());

    let mut tx = pool.begin().await.unwrap();
    let mut success_ids = Vec::new();
    let mut upload_status = Vec::new();
    let mut remote_changes = Vec::new();
    crate::routes::sync::process_todo_changes(
        &mut tx,
        "user-1",
        "client-1",
        &resolved_icons,
        Utc::now(),
        &changes,
        &mut success_ids,
        &mut upload_status,
        &mut remote_changes,
    )
    .await
    .expect("todo processing should succeed");
    tx.commit().await.unwrap();

    let row = sqlx::query!(
        "SELECT icon, updated_by_client FROM todo_items WHERE id = $1",
        "todo-icon-1"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.icon, Some("Plumbing".to_string()));
    // `SERVER-AI` is what makes the icon come back to the sending device as a remote mutation.
    assert_eq!(row.updated_by_client, Some("SERVER-AI".to_string()));
}

/// An account with no AI budget left still syncs; it just gets no icon.
///
/// The budget refusal must stay swallowed: the icon is a garnish, and turning a spend
/// limit into a failed sync would turn it into data loss. Exercised through the real
/// handler so the swallowing is pinned end to end.
#[sqlx::test]
async fn test_sync_succeeds_without_icon_when_budget_is_exhausted(pool: PgPool) {
    use redis::AsyncCommands;

    let state = setup_state(pool.clone());

    // A user of this test's own, so the exhausted counter cannot affect (or be affected
    // by) anything else sharing this Redis.
    let user_id = format!("budget-user-{}", uuid::Uuid::new_v4());
    let counter_key = format!(
        "ai:gemini:calls:user:{}:{}",
        user_id,
        Utc::now().format("%Y-%m-%d")
    );
    let mut conn = state
        .redis_client
        .get_multiplexed_async_connection()
        .await
        .unwrap();
    // Above any configured cap, so the charge is refused whatever the limits are set to.
    let _: () = conn.set(&counter_key, i64::MAX - 1).await.unwrap();

    let todo_data = TodoItemData {
        id: "todo-nobudget-1".to_string(),
        title: "Fix the kitchen tap".to_string(),
        is_completed: false,
        created_at: 0,
        position: 0,
        scheduled_date: None,
        recurrence_rule: None,
        scheduled_at: 0,
        user_id: Some(user_id.clone()),
        parent_id: None,
        is_daily: false,
        due_date: None,
        description: None,
        list_id: None,
        priority: 0,
        icon: None,
        sync_state: "SYNCED".to_string(),
        version: 1,
        is_deleted: false,
    };
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
        scope: Some(SyncScope::Todo),
        todo_list_changes: vec![],
        todo_changes: vec![TodoChangeDelta {
            id: "todo-nobudget-1".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&todo_data).unwrap()),
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
    };

    let claims = crate::auth::tokens::Claims {
        sub: user_id.clone(),
        client_uuid: "client-1".to_string(),
        exp: 10000000000,
    };
    let res = crate::routes::sync::sync_handler(
        State(state),
        axum::Extension(claims),
        crate::routes::sync::AppJson(req),
    )
    .await
    .expect("A spent AI budget must not fail the sync")
    .0;

    assert_eq!(res.success_ids, vec!["todo-nobudget-1"]);

    let row = sqlx::query!(
        "SELECT icon, updated_by_client FROM todo_items WHERE id = $1",
        "todo-nobudget-1"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.icon, None);
    // Not `SERVER-AI`: the server assigned nothing, so this is the client's own write.
    assert_eq!(row.updated_by_client, Some("client-1".to_string()));

    let _: () = conn.del(&counter_key).await.unwrap();
}

/// The batch-size gate is applied before anything is spent: a batch of three or more
/// resolves to no icons at all, without touching Redis or Gemini.
#[sqlx::test]
async fn test_resolve_todo_icons_skips_large_batches(pool: PgPool) {
    let state = setup_state(pool.clone());
    let changes: Vec<TodoChangeDelta> = (0..3)
        .map(|i| {
            let todo_data = TodoItemData {
                id: format!("todo-batch-{}", i),
                title: format!("Task {}", i),
                is_completed: false,
                created_at: 0,
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
                icon: None,
                sync_state: "SYNCED".to_string(),
                version: 1,
                is_deleted: false,
            };
            TodoChangeDelta {
                id: format!("todo-batch-{}", i),
                operation_type: OperationType::Insert,
                version: 1,
                data: Some(serde_json::to_value(&todo_data).unwrap()),
            }
        })
        .collect();

    let resolved =
        crate::routes::sync::todo::icons::resolve_todo_icons(&state, "user-1", &changes).await;
    assert!(resolved.is_empty());
}
