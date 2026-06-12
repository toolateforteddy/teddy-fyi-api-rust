use super::*;
use crate::state::AppState;
use axum::{extract::State, Json};
use chrono::Utc;
use sqlx::PgPool;
use std::sync::Arc;

fn setup_state(pool: PgPool) -> AppState {
    AppState {
        client_id: "test-client".to_string(),
        google_client: Arc::new(google_oauth::AsyncClient::new("test-client")),
        db_pool: pool,
        jwt_secret: "test-secret".to_string(),
        gemini_api_key: "test-key".to_string(),
    }
}

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
    };

    let res = sync_handler(State(state), Json(req))
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
    };

    let res = sync_handler(State(state), Json(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res.success_ids, vec!["todo-1"]);
}

#[sqlx::test]
async fn test_sync_handler_update_todo(pool: PgPool) {
    sqlx::query!(
        "INSERT INTO todo_items (id, title, \"isCompleted\", \"createdAt\", position, \"scheduledAt\", \"isDaily\", priority, icon, sync_state, version, updated_by_client, is_deleted)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        "todo-2", "Test Todo", false, 0_i64, 0_i32, 0_i64, false, 0_i32, None::<String>, "SYNCED", 1_i32, "client-1", false
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = setup_state(pool.clone());
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-2".to_string(),
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
    };

    let res = sync_handler(State(state), Json(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res.success_ids, vec!["todo-2"]);

    let updated = sqlx::query!(
        "SELECT version, updated_by_client FROM todo_items WHERE id = $1",
        "todo-2"
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert_eq!(updated.version, 2);
    assert_eq!(updated.updated_by_client, Some("client-2".to_string()));
}

#[sqlx::test]
async fn test_sync_handler_delete_todo(pool: PgPool) {
    sqlx::query!(
        "INSERT INTO todo_items (id, title, \"isCompleted\", \"createdAt\", position, \"scheduledAt\", \"isDaily\", priority, icon, sync_state, version, updated_by_client, is_deleted)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        "todo-3", "Test Todo", false, 0_i64, 0_i32, 0_i64, false, 0_i32, None::<String>, "SYNCED", 1_i32, "client-1", false
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = setup_state(pool.clone());
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-2".to_string(),
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
    };

    let res = sync_handler(State(state), Json(req))
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
async fn test_sync_handler_remote_mutations(pool: PgPool) {
    // Insert an old record (not fetched)
    sqlx::query!(
        "INSERT INTO todo_items (id, title, \"isCompleted\", \"createdAt\", position, \"scheduledAt\", \"isDaily\", priority, icon, sync_state, version, updated_by_client, updated_at, is_deleted)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, NOW() - INTERVAL '1 hour', $13)",
        "todo-old", "Old", false, 0_i64, 0_i32, 0_i64, false, 0_i32, None::<String>, "SYNCED", 1_i32, "client-1", false
    )
    .execute(&pool)
    .await
    .unwrap();

    // Insert a new record (should be fetched)
    sqlx::query!(
        "INSERT INTO todo_items (id, title, \"isCompleted\", \"createdAt\", position, \"scheduledAt\", \"isDaily\", priority, icon, sync_state, version, updated_by_client, updated_at, is_deleted)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, NOW(), $13)",
        "todo-new", "New", false, 0_i64, 0_i32, 0_i64, false, 0_i32, None::<String>, "SYNCED", 2_i32, "client-1", false
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = setup_state(pool.clone());
    let last_synced = Utc::now() - chrono::Duration::minutes(30);

    let req = SyncRequest {
        last_synced_at: Some(last_synced),
        client_id: "client-2".to_string(), // different client id, so it gets the changes
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
    };

    let res = sync_handler(State(state), Json(req))
        .await
        .expect("Handler should succeed")
        .0;

    // Should only fetch the "todo-new" since "todo-old" is older than 30 mins
    assert_eq!(res.remote_todo_changes.len(), 1);
    assert_eq!(res.remote_todo_changes[0].id, "todo-new");
    assert_eq!(res.remote_todo_changes[0].version, 2);
}

#[sqlx::test]
async fn test_sync_handler_grocery_lists(pool: PgPool) {
    let state = setup_state(pool.clone());

    // 1. Test Insert
    let list_data = GroceryListData {
        id: "glist-1".to_string(),
        name: "My Grocery List".to_string(),
        owner_id: Some("owner-1".to_string()),
        created_at: 123456789,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![GroceryListChangeDelta {
            id: "glist-1".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&list_data).unwrap()),
        }],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
    };

    let res = sync_handler(State(state.clone()), Json(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res.success_ids, vec!["glist-1"]);

    // 2. Test Update (Base Client Version = 2. DB has 1. std::cmp::max(1, 2) + 1 = 3)
    let updated_list_data = GroceryListData {
        id: "glist-1".to_string(),
        name: "Updated Grocery List".to_string(),
        owner_id: Some("owner-1".to_string()),
        created_at: 123456789,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };
    let req_update = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![GroceryListChangeDelta {
            id: "glist-1".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&updated_list_data).unwrap()),
        }],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
    };

    let res_update = sync_handler(State(state.clone()), Json(req_update))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res_update.success_ids, vec!["glist-1"]);

    let db_row = sqlx::query!(
        "SELECT name, version FROM grocery_lists WHERE id = $1",
        "glist-1"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(db_row.name, "Updated Grocery List");
    assert_eq!(db_row.version, 3);

    // 3. Test Delete
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![GroceryListChangeDelta {
            id: "glist-1".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
    };

    let res_delete = sync_handler(State(state.clone()), Json(req_delete))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res_delete.success_ids, vec!["glist-1"]);

    let db_row = sqlx::query!(
        "SELECT is_deleted FROM grocery_lists WHERE id = $1",
        "glist-1"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(db_row.is_deleted);
}

#[sqlx::test]
async fn test_sync_handler_grocery_list_members(pool: PgPool) {
    let state = setup_state(pool.clone());

    // Pre-insert grocery list so the member foreign key constraint is satisfied
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
        "glist-2",
        "Test List",
        0_i64,
        1_i32,
        false,
        "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 1. Test Insert
    let member_data = GroceryListMemberData {
        id: "member-1".to_string(),
        list_id: "glist-2".to_string(),
        user_id: "user-123".to_string(),
        role: "ADMIN".to_string(),
        joined_at: 123456,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![GroceryListMemberChangeDelta {
            id: "member-1".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&member_data).unwrap()),
        }],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
    };

    let res = sync_handler(State(state.clone()), Json(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res.success_ids, vec!["member-1"]);

    // 2. Test Update (Base Client Version = 2. DB has 1. std::cmp::max(1, 2) + 1 = 3)
    let updated_member_data = GroceryListMemberData {
        id: "member-1".to_string(),
        list_id: "glist-2".to_string(),
        user_id: "user-123".to_string(),
        role: "MEMBER".to_string(),
        joined_at: 123456,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };
    let req_update = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![GroceryListMemberChangeDelta {
            id: "member-1".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&updated_member_data).unwrap()),
        }],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
    };

    let res_update = sync_handler(State(state.clone()), Json(req_update))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res_update.success_ids, vec!["member-1"]);

    let db_row = sqlx::query!(
        "SELECT role, version FROM grocery_list_members WHERE id = $1",
        "member-1"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(db_row.role, "MEMBER");
    assert_eq!(db_row.version, 3);

    // 3. Test Delete
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![GroceryListMemberChangeDelta {
            id: "member-1".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
    };

    let res_delete = sync_handler(State(state.clone()), Json(req_delete))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res_delete.success_ids, vec!["member-1"]);

    let db_row = sqlx::query!(
        "SELECT is_deleted FROM grocery_list_members WHERE id = $1",
        "member-1"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(db_row.is_deleted);
}

#[sqlx::test]
async fn test_sync_handler_stores_and_categories(pool: PgPool) {
    let state = setup_state(pool.clone());

    // 1. Test Stores Insert
    let store_data = StoreData {
        id: 10,
        name: "Supermarket".to_string(),
        position: 1,
        is_default_supported: true,
        user_id: None,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };
    // Test Categories Insert
    let category_data = CategoryData {
        id: 20,
        name: "Produce".to_string(),
        position: 2,
        user_id: None,
        icon: None,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![StoreChangeDelta {
            id: 10,
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&store_data).unwrap()),
        }],
        category_changes: vec![CategoryChangeDelta {
            id: 20,
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&category_data).unwrap()),
        }],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
    };

    let res = sync_handler(State(state.clone()), Json(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res.success_ids.contains(&"10".to_string()));
    assert!(res.success_ids.contains(&"20".to_string()));

    // 2. Test Stores & Categories Update
    let updated_store = StoreData {
        id: 10,
        name: "Updated Supermarket".to_string(),
        position: 1,
        is_default_supported: true,
        user_id: None,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };
    let updated_category = CategoryData {
        id: 20,
        name: "Updated Produce".to_string(),
        position: 2,
        user_id: None,
        icon: None,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };
    let req_update = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![StoreChangeDelta {
            id: 10,
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&updated_store).unwrap()),
        }],
        category_changes: vec![CategoryChangeDelta {
            id: 20,
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&updated_category).unwrap()),
        }],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
    };

    let res_update = sync_handler(State(state.clone()), Json(req_update))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res_update.success_ids.contains(&"10".to_string()));
    assert!(res_update.success_ids.contains(&"20".to_string()));

    let db_store = sqlx::query!("SELECT name FROM stores WHERE id = $1", 10)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(db_store.name, "Updated Supermarket");

    let db_cat = sqlx::query!("SELECT name FROM categories WHERE id = $1", 20)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(db_cat.name, "Updated Produce");

    // 3. Test Delete
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![StoreChangeDelta {
            id: 10,
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        category_changes: vec![CategoryChangeDelta {
            id: 20,
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
    };

    let res_delete = sync_handler(State(state.clone()), Json(req_delete))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res_delete.success_ids.contains(&"10".to_string()));
    assert!(res_delete.success_ids.contains(&"20".to_string()));

    let db_store = sqlx::query!("SELECT is_deleted FROM stores WHERE id = $1", 10)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(db_store.is_deleted);

    let db_cat = sqlx::query!("SELECT is_deleted FROM categories WHERE id = $1", 20)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(db_cat.is_deleted);
}

#[sqlx::test]
async fn test_sync_handler_grocery_items_and_store_info(pool: PgPool) {
    let state = setup_state(pool.clone());

    // Pre-create grocery list and store
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
        "glist-3",
        "Test List",
        0_i64,
        1_i32,
        false,
        "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO stores (id, name, position, \"isDefaultSupported\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        100, "Test Store", 1, true, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 1. Test Insert
    let item_data = GroceryItemData {
        id: 50,
        name: "Apples".to_string(),
        quantity: "5".to_string(),
        is_bought: false,
        created_at: 1000,
        position: 1,
        category_id: None,
        times_bought: 0,
        user_id: None,
        is_active: true,
        list_id: Some("glist-3".to_string()),
        unit: None,
        notes: None,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let store_info = GroceryItemStoreInfoData {
        grocery_item_id: 50,
        store_id: 100,
        price: Some(1.99),
        is_available: true,
        user_id: None,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![GroceryChangeDelta {
            id: 50,
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&item_data).unwrap()),
        }],
        grocery_item_store_info_changes: vec![GroceryItemStoreInfoChangeDelta {
            grocery_item_id: 50,
            store_id: 100,
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&store_info).unwrap()),
        }],
    };

    let res = sync_handler(State(state.clone()), Json(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res.success_ids.contains(&"50".to_string()));
    assert!(res.success_ids.contains(&"50-100".to_string()));

    // 2. Test Update
    let updated_item = GroceryItemData {
        id: 50,
        name: "Green Apples".to_string(),
        quantity: "10".to_string(),
        is_bought: true,
        created_at: 1000,
        position: 1,
        category_id: None,
        times_bought: 1,
        user_id: None,
        is_active: true,
        list_id: Some("glist-3".to_string()),
        unit: None,
        notes: None,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let updated_store_info = GroceryItemStoreInfoData {
        grocery_item_id: 50,
        store_id: 100,
        price: Some(2.49),
        is_available: true,
        user_id: None,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let req_update = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![GroceryChangeDelta {
            id: 50,
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&updated_item).unwrap()),
        }],
        grocery_item_store_info_changes: vec![GroceryItemStoreInfoChangeDelta {
            grocery_item_id: 50,
            store_id: 100,
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&updated_store_info).unwrap()),
        }],
    };

    let res_update = sync_handler(State(state.clone()), Json(req_update))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res_update.success_ids.contains(&"50".to_string()));
    assert!(res_update.success_ids.contains(&"50-100".to_string()));

    let db_item = sqlx::query!(
        "SELECT name, quantity, \"isBought\" as is_bought FROM grocery_items WHERE id = $1",
        50
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(db_item.name, "Green Apples");
    assert_eq!(db_item.quantity, "10");
    assert!(db_item.is_bought);

    let db_info = sqlx::query!("SELECT price FROM grocery_item_store_info WHERE \"groceryItemId\" = $1 AND \"storeId\" = $2", 50, 100)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(db_info.price, Some(2.49));

    // 3. Test Delete
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![GroceryChangeDelta {
            id: 50,
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        grocery_item_store_info_changes: vec![GroceryItemStoreInfoChangeDelta {
            grocery_item_id: 50,
            store_id: 100,
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
    };

    let res_delete = sync_handler(State(state.clone()), Json(req_delete))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res_delete.success_ids.contains(&"50".to_string()));
    assert!(res_delete.success_ids.contains(&"50-100".to_string()));

    // Grocery item is soft-deleted, so is_deleted should be true
    let db_deleted_item = sqlx::query!("SELECT is_deleted FROM grocery_items WHERE id = $1", 50)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(db_deleted_item.is_deleted);

    // Store info is soft-deleted, so is_deleted should be true
    let db_deleted_info = sqlx::query!("SELECT is_deleted FROM grocery_item_store_info WHERE \"groceryItemId\" = $1 AND \"storeId\" = $2", 50, 100)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(db_deleted_info.is_deleted);
}

#[sqlx::test]
async fn test_fetch_remote_mutations_by_table(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();

    let client_id = "test-client";
    let other_client = "other-client";
    let last_synced_at = Some(Utc::now() - chrono::Duration::minutes(5));

    // --- 1. todo_lists ---
    sqlx::query!(
        r#"INSERT INTO todo_lists (id, name, "colorHex", "userId", "createdAt", sync_state, version, is_deleted, updated_at, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), $9)"#,
        "todolist-remote-1",
        "Remote List",
        "#FF0000",
        "user-1",
        0_i64,
        "SYNCED",
        1_i32,
        false,
        other_client
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // --- 2. todo_items ---
    sqlx::query!(
        r#"INSERT INTO todo_items (id, title, "isCompleted", "createdAt", position, "scheduledAt", "isDaily", priority, icon, sync_state, version, is_deleted, updated_at, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, NOW(), $13)"#,
        "todoitem-remote-1",
        "Remote Todo",
        false,
        0_i64,
        1_i32,
        0_i64,
        false,
        1_i32,
        None::<String>,
        "SYNCED",
        1_i32,
        false,
        other_client
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // Verify TODO mutations
    let (todo_lists, todo_items) = fetch_remote_todo_mutations(&mut tx, client_id, last_synced_at)
        .await
        .unwrap();

    assert!(todo_lists.iter().any(|d| d.id == "todolist-remote-1"));
    assert!(todo_items.iter().any(|d| d.id == "todoitem-remote-1"));

    // --- 3. grocery_lists ---
    sqlx::query!(
        r#"INSERT INTO grocery_lists (id, name, "ownerId", "createdAt", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, NOW(), $6, $7, $8)"#,
        "grocerylist-remote-1",
        "Remote Grocery List",
        "owner-1",
        0_i64,
        1_i32,
        other_client,
        false,
        "SYNCED"
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // --- 4. grocery_list_members ---
    sqlx::query!(
        r#"INSERT INTO grocery_list_members (id, "listId", "userId", role, "joinedAt", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, NOW(), $7, $8, $9)"#,
        "member-remote-1",
        "grocerylist-remote-1",
        "user-1",
        "MEMBER",
        0_i64,
        1_i32,
        other_client,
        false,
        "SYNCED"
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // --- 5. stores ---
    sqlx::query!(
        r#"INSERT INTO stores (id, name, position, "isDefaultSupported", "userId", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, NOW(), $7, $8, $9)"#,
        1001,
        "Remote Store",
        1,
        true,
        "user-1",
        1_i32,
        other_client,
        false,
        "SYNCED"
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // --- 6. categories ---
    sqlx::query!(
        r#"INSERT INTO categories (id, name, position, "userId", icon, version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, NOW(), $7, $8, $9)"#,
        2001,
        "Remote Category",
        1,
        "user-1",
        None::<String>,
        1_i32,
        other_client,
        false,
        "SYNCED"
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // --- 7. grocery_items ---
    sqlx::query!(
        r#"INSERT INTO grocery_items (id, name, quantity, "isBought", "createdAt", position, "categoryId", "timesBought", "userId", "isActive", "listId", unit, notes, version, is_deleted, updated_at, updated_by_client, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, NOW(), $16, $17)"#,
        3001,
        "Remote Grocery Item",
        "1",
        false,
        0_i64,
        1_i32,
        Some(2001),
        0_i32,
        "user-1",
        true,
        Some("grocerylist-remote-1".to_string()),
        "unit",
        "notes",
        1_i32,
        false,
        other_client,
        "SYNCED"
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // --- 8. grocery_item_store_info ---
    sqlx::query!(
        r#"INSERT INTO grocery_item_store_info ("groceryItemId", "storeId", price, "isAvailable", "userId", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, NOW(), $7, $8, $9)"#,
        3001,
        1001,
        2.99,
        true,
        "user-1",
        1_i32,
        other_client,
        false,
        "SYNCED"
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // Verify Grocery mutations
    let (
        grocery_lists,
        grocery_list_members,
        stores,
        categories,
        grocery_items,
        grocery_item_store_infos,
    ) = fetch_remote_grocery_mutations(&mut tx, client_id, last_synced_at)
        .await
        .unwrap();

    assert!(grocery_lists.iter().any(|d| d.id == "grocerylist-remote-1"));
    assert!(grocery_list_members
        .iter()
        .any(|d| d.id == "member-remote-1"));
    assert!(stores.iter().any(|d| d.id == 1001));
    assert!(categories.iter().any(|d| d.id == 2001));
    assert!(grocery_items.iter().any(|d| d.id == 3001));
    assert!(grocery_item_store_infos
        .iter()
        .any(|d| d.grocery_item_id == 3001 && d.store_id == 1001));

    tx.rollback().await.unwrap();
}

#[sqlx::test]
async fn test_fetch_remote_mutations_echo_prevention(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();

    let client_id = "test-client";
    let last_synced_at = Some(Utc::now() - chrono::Duration::minutes(5));

    // --- 1. todo_lists ---
    sqlx::query!(
        r#"INSERT INTO todo_lists (id, name, "colorHex", "userId", "createdAt", sync_state, version, is_deleted, updated_at, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), $9)"#,
        "todolist-echo-1",
        "Echo List",
        "#FF0000",
        "user-1",
        0_i64,
        "SYNCED",
        1_i32,
        false,
        client_id
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // --- 2. todo_items ---
    sqlx::query!(
        r#"INSERT INTO todo_items (id, title, "isCompleted", "createdAt", position, "scheduledAt", "isDaily", priority, icon, sync_state, version, is_deleted, updated_at, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, NOW(), $13)"#,
        "todoitem-echo-1",
        "Echo Todo",
        false,
        0_i64,
        1_i32,
        0_i64,
        false,
        1_i32,
        None::<String>,
        "SYNCED",
        1_i32,
        false,
        client_id
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // Verify TODO mutations (should be empty as we are the updater)
    let (todo_lists, todo_items) = fetch_remote_todo_mutations(&mut tx, client_id, last_synced_at)
        .await
        .unwrap();

    assert!(!todo_lists.iter().any(|d| d.id == "todolist-echo-1"));
    assert!(!todo_items.iter().any(|d| d.id == "todoitem-echo-1"));

    // --- 3. grocery_lists ---
    sqlx::query!(
        r#"INSERT INTO grocery_lists (id, name, "ownerId", "createdAt", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, NOW(), $6, $7, $8)"#,
        "grocerylist-echo-1",
        "Echo Grocery List",
        "owner-1",
        0_i64,
        1_i32,
        client_id,
        false,
        "SYNCED"
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // --- 4. grocery_list_members ---
    sqlx::query!(
        r#"INSERT INTO grocery_list_members (id, "listId", "userId", role, "joinedAt", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, NOW(), $7, $8, $9)"#,
        "member-echo-1",
        "grocerylist-echo-1",
        "user-1",
        "MEMBER",
        0_i64,
        1_i32,
        client_id,
        false,
        "SYNCED"
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // --- 5. stores ---
    sqlx::query!(
        r#"INSERT INTO stores (id, name, position, "isDefaultSupported", "userId", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, NOW(), $7, $8, $9)"#,
        1002,
        "Echo Store",
        1,
        true,
        "user-1",
        1_i32,
        client_id,
        false,
        "SYNCED"
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // --- 6. categories ---
    sqlx::query!(
        r#"INSERT INTO categories (id, name, position, "userId", icon, version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, NOW(), $7, $8, $9)"#,
        2002,
        "Echo Category",
        1,
        "user-1",
        None::<String>,
        1_i32,
        client_id,
        false,
        "SYNCED"
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // --- 7. grocery_items ---
    sqlx::query!(
        r#"INSERT INTO grocery_items (id, name, quantity, "isBought", "createdAt", position, "categoryId", "timesBought", "userId", "isActive", "listId", unit, notes, version, is_deleted, updated_at, updated_by_client, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, NOW(), $16, $17)"#,
        3002,
        "Echo Grocery Item",
        "1",
        false,
        0_i64,
        1_i32,
        Some(2002),
        0_i32,
        "user-1",
        true,
        Some("grocerylist-echo-1".to_string()),
        "unit",
        "notes",
        1_i32,
        false,
        client_id,
        "SYNCED"
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // --- 8. grocery_item_store_info ---
    sqlx::query!(
        r#"INSERT INTO grocery_item_store_info ("groceryItemId", "storeId", price, "isAvailable", "userId", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, NOW(), $7, $8, $9)"#,
        3002,
        1002,
        2.99,
        true,
        "user-1",
        1_i32,
        client_id,
        false,
        "SYNCED"
    )
    .execute(&mut *tx)
    .await
    .unwrap();

    // Verify Grocery mutations (should be empty as we are the updater)
    let (
        grocery_lists,
        grocery_list_members,
        stores,
        categories,
        grocery_items,
        grocery_item_store_infos,
    ) = fetch_remote_grocery_mutations(&mut tx, client_id, last_synced_at)
        .await
        .unwrap();

    assert!(!grocery_lists.iter().any(|d| d.id == "grocerylist-echo-1"));
    assert!(!grocery_list_members.iter().any(|d| d.id == "member-echo-1"));
    assert!(!stores.iter().any(|d| d.id == 1002));
    assert!(!categories.iter().any(|d| d.id == 2002));
    assert!(!grocery_items.iter().any(|d| d.id == 3002));
    assert!(!grocery_item_store_infos
        .iter()
        .any(|d| d.grocery_item_id == 3002 && d.store_id == 1002));

    tx.rollback().await.unwrap();
}

#[test]
fn test_sync_request_deserialization_defaults() {
    let json_data = r#"{
        "client_id": "test-client-id"
    }"#;

    let req: SyncRequest = serde_json::from_str(json_data).expect("Should deserialize successfully with missing fields");
    assert_eq!(req.client_id, "test-client-id");
    assert!(req.last_synced_at.is_none());
    assert!(req.todo_list_changes.is_empty());
    assert!(req.todo_changes.is_empty());
    assert!(req.grocery_list_changes.is_empty());
    assert!(req.grocery_list_member_changes.is_empty());
    assert!(req.store_changes.is_empty());
    assert!(req.category_changes.is_empty());
    assert!(req.grocery_changes.is_empty());
    assert!(req.grocery_item_store_info_changes.is_empty());
}
