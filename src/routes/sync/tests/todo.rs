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
