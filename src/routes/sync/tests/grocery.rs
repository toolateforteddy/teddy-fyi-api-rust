use sqlx::PgPool;
use axum::extract::State;
use axum::Extension;
use chrono::Utc;
use crate::routes::sync::tests::helpers::{setup_state, sync_handler};
use crate::routes::sync::{
    SyncRequest, SyncScope, GroceryListData, GroceryListChangeDelta, OperationType,
    GroceryListMemberData, GroceryListMemberChangeDelta, StoreData, StoreChangeDelta,
    CategoryData, CategoryChangeDelta, GroceryItemData, GroceryChangeDelta,
    GroceryItemStoreInfoData, GroceryItemStoreInfoChangeDelta, AppJson, AppError
};
use crate::auth::tokens::Claims;

#[sqlx::test]
async fn test_sync_handler_grocery_lists(pool: PgPool) {
    let state = setup_state(pool.clone());

    // 1. Test Insert
    let list_data = GroceryListData {
        id: "glist-1".to_string(),
        name: "My Grocery List".to_string(),
        owner_id: Some("user-1".to_string()),
        created_at: 123456789,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        scope: None,
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
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res.success_ids, vec!["glist-1"]);

    // 2. Test Update (Base Client Version = 2. DB has 1. std::cmp::max(1, 2) + 1 = 3)
    let updated_list_data = GroceryListData {
        id: "glist-1".to_string(),
        name: "Updated Grocery List".to_string(),
        owner_id: Some("user-1".to_string()),
        created_at: 123456789,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };
    let req_update = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        scope: None,
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
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res_update = sync_handler(State(state.clone()), AppJson(req_update))
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
        scope: None,
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
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res_delete = sync_handler(State(state.clone()), AppJson(req_delete))
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

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "glist-2-owner", "glist-2", "user-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
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
        scope: None,
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
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res = sync_handler(State(state.clone()), AppJson(req))
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
        scope: None,
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
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res_update = sync_handler(State(state.clone()), AppJson(req_update))
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
        scope: None,
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
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res_delete = sync_handler(State(state.clone()), AppJson(req_delete))
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
        id: "10".to_string(),
        name: "Supermarket".to_string(),
        position: 1,
        is_default_supported: true,
        user_id: None,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
        list_id: None,
    };
    // Test Categories Insert
    let category_data = CategoryData {
        id: "20".to_string(),
        name: "Produce".to_string(),
        position: 2,
        user_id: None,
        icon: None,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
        list_id: None,
    };

    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![StoreChangeDelta {
            id: "10".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&store_data).unwrap()),
        }],
        category_changes: vec![CategoryChangeDelta {
            id: "20".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&category_data).unwrap()),
        }],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res.success_ids.contains(&"10".to_string()));
    assert!(res.success_ids.contains(&"20".to_string()));

    // 2. Test Stores & Categories Update
    let updated_store = StoreData {
        id: "10".to_string(),
        name: "Updated Supermarket".to_string(),
        position: 1,
        is_default_supported: true,
        user_id: None,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
        list_id: None,
    };
    let updated_category = CategoryData {
        id: "20".to_string(),
        name: "Updated Produce".to_string(),
        position: 2,
        user_id: None,
        icon: None,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
        list_id: None,
    };
    let req_update = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![StoreChangeDelta {
            id: "10".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&updated_store).unwrap()),
        }],
        category_changes: vec![CategoryChangeDelta {
            id: "20".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&updated_category).unwrap()),
        }],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res_update = sync_handler(State(state.clone()), AppJson(req_update))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res_update.success_ids.contains(&"10".to_string()));
    assert!(res_update.success_ids.contains(&"20".to_string()));

    let db_store = sqlx::query!("SELECT name FROM stores WHERE id = $1", "10")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(db_store.name, "Updated Supermarket");

    let db_cat = sqlx::query!("SELECT name FROM categories WHERE id = $1", "20")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(db_cat.name, "Updated Produce");

    // 3. Test Delete
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![StoreChangeDelta {
            id: "10".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        category_changes: vec![CategoryChangeDelta {
            id: "20".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res_delete = sync_handler(State(state.clone()), AppJson(req_delete))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res_delete.success_ids.contains(&"10".to_string()));
    assert!(res_delete.success_ids.contains(&"20".to_string()));

    let db_store = sqlx::query!("SELECT is_deleted FROM stores WHERE id = $1", "10")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(db_store.is_deleted);

    let db_cat = sqlx::query!("SELECT is_deleted FROM categories WHERE id = $1", "20")
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
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "glist-3-member", "glist-3", "user-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO stores (id, name, position, \"isDefaultSupported\", \"userId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "100", "Test Store", 1, true, "user-1", 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 1. Test Insert
    let item_data = GroceryItemData {
        id: "50".to_string(),
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
        id: "50-100".to_string(),
        grocery_item_id: "50".to_string(),
        store_id: "100".to_string(),
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
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![GroceryChangeDelta {
            id: "50".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&item_data).unwrap()),
        }],
        grocery_item_store_info_changes: vec![GroceryItemStoreInfoChangeDelta {
            id: "50-100".to_string(),
            grocery_item_id: "50".to_string(),
            store_id: "100".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&store_info).unwrap()),
        }],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res.success_ids.contains(&"50".to_string()));
    assert!(res.success_ids.contains(&"50-100".to_string()));

    // 2. Test Update
    let updated_item = GroceryItemData {
        id: "50".to_string(),
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
        id: "50-100".to_string(),
        grocery_item_id: "50".to_string(),
        store_id: "100".to_string(),
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
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![GroceryChangeDelta {
            id: "50".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&updated_item).unwrap()),
        }],
        grocery_item_store_info_changes: vec![GroceryItemStoreInfoChangeDelta {
            id: "50-100".to_string(),
            grocery_item_id: "50".to_string(),
            store_id: "100".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&updated_store_info).unwrap()),
        }],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res_update = sync_handler(State(state.clone()), AppJson(req_update))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res_update.success_ids.contains(&"50".to_string()));
    assert!(res_update.success_ids.contains(&"50-100".to_string()));

    let db_item = sqlx::query!(
        "SELECT name, quantity, \"isBought\" as is_bought FROM grocery_items WHERE id = $1",
        "50"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(db_item.name, "Green Apples");
    assert_eq!(db_item.quantity, "10");
    assert!(db_item.is_bought);

    let db_info = sqlx::query!("SELECT price FROM grocery_item_store_info WHERE \"groceryItemId\" = $1 AND \"storeId\" = $2", "50", "100")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(db_info.price, Some(2.49));

    // 3. Test Delete
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![GroceryChangeDelta {
            id: "50".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        grocery_item_store_info_changes: vec![GroceryItemStoreInfoChangeDelta {
            id: "50-100".to_string(),
            grocery_item_id: "50".to_string(),
            store_id: "100".to_string(),
            operation_type: OperationType::Delete,
            version: 3,
            data: None,
        }],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res_delete = sync_handler(State(state.clone()), AppJson(req_delete))
        .await
        .expect("Handler should succeed")
        .0;
    assert!(res_delete.success_ids.contains(&"50".to_string()));
    assert!(res_delete.success_ids.contains(&"50-100".to_string()));

    // Grocery item is soft-deleted, so is_deleted should be true
    let db_deleted_item = sqlx::query!("SELECT is_deleted FROM grocery_items WHERE id = $1", "50")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(db_deleted_item.is_deleted);

    // Store info is soft-deleted, so is_deleted should be true
    let db_deleted_info = sqlx::query!("SELECT is_deleted FROM grocery_item_store_info WHERE \"groceryItemId\" = $1 AND \"storeId\" = $2", "50", "100")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(db_deleted_info.is_deleted);
}

#[sqlx::test]
async fn test_sync_handler_scope_grocery(pool: PgPool) {
    let state = setup_state(pool.clone());
    let other_client = "other-client";
    
    // Todo List
    sqlx::query!(
        r#"INSERT INTO todo_lists (id, name, "colorHex", "userId", "createdAt", sync_state, version, is_deleted, updated_at, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), $9)"#,
        "todolist-scope-1",
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
        "grocerylist-scope-1",
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

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "glist-scope-member-1", "grocerylist-scope-1", "user-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        client_id: "client-1".to_string(),
        scope: Some(SyncScope::Grocery),
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

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    assert!(res.remote_grocery_list_changes.iter().any(|d| d.id == "grocerylist-scope-1"));
    assert!(res.remote_todo_list_changes.is_empty());
}

#[sqlx::test]
async fn test_sync_unauthorized_grocery_list_access(pool: PgPool) {
    let state = setup_state(pool.clone());
    
    // Insert a grocery list with user-2 as member only.
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
        "glist-forbidden-1",
        "Forbidden List",
        0_i64,
        1_i32,
        false,
        "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "glist-forbidden-member", "glist-forbidden-1", "user-2", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Now, let's call sync_handler (which uses claims for user-1) trying to modify glist-forbidden-1
    let list_data = GroceryListData {
        id: "glist-forbidden-1".to_string(),
        name: "Attempting Modify".to_string(),
        owner_id: Some("owner-1".to_string()),
        created_at: 123456789,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };
    
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![GroceryListChangeDelta {
            id: "glist-forbidden-1".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&list_data).unwrap()),
        }],
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

    let err = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect_err("Handler should fail with Forbidden");

    assert!(matches!(err, AppError::Forbidden(_)));
}

#[sqlx::test]
async fn test_sync_unauthorized_grocery_item_access(pool: PgPool) {
    let state = setup_state(pool.clone());
    
    // Insert a grocery list with user-2 as member only.
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
        "glist-forbidden-2",
        "Forbidden List",
        0_i64,
        1_i32,
        false,
        "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "glist-forbidden-member-2", "glist-forbidden-2", "user-2", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Now, let's call sync_handler (which uses claims for user-1) trying to insert an item for glist-forbidden-2
    let item_data = GroceryItemData {
        id: "999".to_string(),
        name: "Forbidden Item".to_string(),
        quantity: "1".to_string(),
        is_bought: false,
        created_at: 1000,
        position: 1,
        category_id: None,
        times_bought: 0,
        user_id: None,
        is_active: true,
        list_id: Some("glist-forbidden-2".to_string()),
        unit: None,
        notes: None,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };
    
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![GroceryChangeDelta {
            id: "999".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&item_data).unwrap()),
        }],
        grocery_item_store_info_changes: vec![],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let err = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect_err("Handler should fail with Forbidden");

    assert!(matches!(err, AppError::Forbidden(_)));
}

#[sqlx::test]
async fn test_sync_grocery_item_store_mapping_auto_population(pool: PgPool) {
    let state = setup_state(pool.clone());

    // 1. Create list-alpha and list-beta
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
        "list-alpha", "Alpha List", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
        "list-beta", "Beta List", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 2. Add user-1 as member to both lists
    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "member-alpha", "list-alpha", "user-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "member-beta", "list-beta", "user-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 3. Create a store owned by user-1
    sqlx::query!(
        "INSERT INTO stores (id, name, position, \"isDefaultSupported\", \"userId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "500", "Store Omega", 1, true, "user-1", 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 4. Create an item "Milk" in list-alpha, and map it to Store Omega
    sqlx::query!(
        "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \"categoryId\", \"timesBought\", \"userId\", \"isActive\", \"listId\", unit, notes, version, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        "600", "Milk", "1", false, 0_i64, 1_i32, None::<String>, 0_i32, "user-1", true, Some("list-alpha".to_string()), None::<String>, None::<String>, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_item_store_info (\"groceryItemId\", \"storeId\", price, \"isAvailable\", \"userId\", version, is_deleted, sync_state, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        "600", "500", 2.99, true, "user-1", 1_i32, false, "SYNCED", "client-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 5. Sync-upload a new item "milk" (lowercase, exact match case-insensitive) in list-beta
    let item_data = GroceryItemData {
        id: "700".to_string(),
        name: "milk".to_string(),
        quantity: "2".to_string(),
        is_bought: false,
        created_at: 2000,
        position: 2,
        category_id: None,
        times_bought: 0,
        user_id: None,
        is_active: true,
        list_id: Some("list-beta".to_string()),
        unit: None,
        notes: None,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![GroceryChangeDelta {
            id: "700".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&item_data).unwrap()),
        }],
        grocery_item_store_info_changes: vec![],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("Sync should succeed")
        .0;

    assert!(res.success_ids.contains(&"700".to_string()));

    // 6. Verify that grocery_item_store_info has been auto-populated for item 700 and store 500
    let mapping = sqlx::query!(
        "SELECT \"groceryItemId\" as grocery_item_id, \"storeId\" as store_id, price, \"isAvailable\" as is_available, \"userId\" as user_id, version, is_deleted, updated_by_client
         FROM grocery_item_store_info
         WHERE \"groceryItemId\" = $1 AND \"storeId\" = $2",
        "700",
        "500"
    )
    .fetch_one(&pool)
    .await
    .expect("Auto-populated store mapping should exist");

    assert_eq!(mapping.price, Some(2.99));
    assert!(mapping.is_available);
    assert_eq!(mapping.user_id, Some("user-1".to_string()));
    assert_eq!(mapping.version, 1);
    assert!(!mapping.is_deleted);
    // MUST be NULL/None so it syncs back to client
    assert_eq!(mapping.updated_by_client, None);
}

#[sqlx::test]
async fn test_sync_grocery_items_without_list_id(pool: PgPool) {
    let state = setup_state(pool.clone());
    
    // 1. Insert item-1 with NULL listId owned by user-1 (updated by other-client)
    sqlx::query!(
        "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \"categoryId\", \"timesBought\", \"userId\", \"isActive\", \"listId\", unit, notes, version, is_deleted, updated_by_client, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NULL, $11, $12, $13, $14, $15, NOW())",
        "801", "No List Item 1", "1", false, 0_i64, 1_i32, None::<String>, 0_i32, "user-1", true, None::<String>, None::<String>, 1_i32, false, "other-client"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 2. Insert item-2 with NULL listId owned by user-2 (updated by other-client)
    sqlx::query!(
        "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \"categoryId\", \"timesBought\", \"userId\", \"isActive\", \"listId\", unit, notes, version, is_deleted, updated_by_client, updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, NULL, $11, $12, $13, $14, $15, NOW())",
        "802", "No List Item 2", "1", false, 0_i64, 1_i32, None::<String>, 0_i32, "user-2", true, None::<String>, None::<String>, 1_i32, false, "other-client"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 3. Call sync_handler for user-1
    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        client_id: "client-1".to_string(),
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
    };

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Sync should succeed")
        .0;

    // 4. Verify user-1 receives item-1 but NOT item-2
    let received_ids: Vec<String> = res.remote_grocery_changes.iter().map(|c| c.id.clone()).collect();
    assert!(received_ids.contains(&"801".to_string()));
    assert!(!received_ids.contains(&"802".to_string()));
}

#[sqlx::test]
async fn test_grocery_list_delete_cascade(pool: PgPool) {
    let state = setup_state(pool.clone());

    // Pre-insert grocery list and associated records
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
        "glist-cascade", "Cascade List", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "cascade-member", "glist-cascade", "user-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \"timesBought\", \"userId\", \"isActive\", \"listId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        "800", "Apples", "5", false, 0_i64, 1, 0, "user-1", true, Some("glist-cascade".to_string()), 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO list_invites (code, \"listId\", \"createdBy\", \"expiresAt\") VALUES ($1, $2, $3, NOW() + INTERVAL '24 hours')",
        "INVITE12", "glist-cascade", "user-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Deleting the list
    let claims = Claims {
        sub: "user-1".to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10000000000,
    };
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        scope: Some(SyncScope::Grocery),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![GroceryListChangeDelta {
            id: "glist-cascade".to_string(),
            operation_type: OperationType::Delete,
            version: 1,
            data: None,
        }],
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

    let res = crate::routes::sync::sync_handler(State(state.clone()), Extension(claims), AppJson(req_delete))
        .await
        .expect("Delete should succeed")
        .0;

    assert!(res.success_ids.contains(&"glist-cascade".to_string()));

    // Verify grocery list is soft-deleted
    let list_db = sqlx::query!("SELECT is_deleted FROM grocery_lists WHERE id = $1", "glist-cascade")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(list_db.is_deleted);

    // Verify associated items are soft-deleted
    let item_db = sqlx::query!("SELECT is_deleted FROM grocery_items WHERE id = $1", "800")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(item_db.is_deleted);

    // Verify members are soft-deleted
    let member_db = sqlx::query!("SELECT is_deleted FROM grocery_list_members WHERE id = $1", "cascade-member")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(member_db.is_deleted);

    // Verify invites are hard-deleted
    let invites_count = sqlx::query!("SELECT count(*) FROM list_invites WHERE \"listId\" = $1", "glist-cascade")
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
    assert_eq!(invites_count, 0);
}

#[sqlx::test]
async fn test_grocery_list_cascade_delete_conflict(pool: PgPool) {
    let state = setup_state(pool.clone());

    // Pre-insert grocery list and associated records
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
        "glist-cascade-conflict", "Cascade Conflict List", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "cascade-conflict-member", "glist-cascade-conflict", "user-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO categories (id, name, position, \"userId\", \"listId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "cat-cascade-conflict", "Fruit", 1, "user-1", Some("glist-cascade-conflict".to_string()), 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO stores (id, name, position, \"isDefaultSupported\", \"userId\", \"listId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        "store-cascade-conflict", "Store A", 1, true, "user-1", Some("glist-cascade-conflict".to_string()), 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \"timesBought\", \"userId\", \"isActive\", \"listId\", \"categoryId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
        "item-cascade-conflict", "Apples", "5", false, 0_i64, 1, 0, "user-1", true, Some("glist-cascade-conflict".to_string()), Some("cat-cascade-conflict".to_string()), 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_item_store_info (\"groceryItemId\", \"storeId\", price, \"isAvailable\", \"userId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "item-cascade-conflict", "store-cascade-conflict", 1.99, true, "user-1", 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Deleting all of them in the same request
    let claims = Claims {
        sub: "user-1".to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10000000000,
    };
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        scope: Some(SyncScope::Grocery),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![GroceryListChangeDelta {
            id: "glist-cascade-conflict".to_string(),
            operation_type: OperationType::Delete,
            version: 1,
            data: None,
        }],
        grocery_list_member_changes: vec![GroceryListMemberChangeDelta {
            id: "cascade-conflict-member".to_string(),
            operation_type: OperationType::Delete,
            version: 1,
            data: None,
        }],
        store_changes: vec![StoreChangeDelta {
            id: "store-cascade-conflict".to_string(),
            operation_type: OperationType::Delete,
            version: 1,
            data: None,
        }],
        category_changes: vec![CategoryChangeDelta {
            id: "cat-cascade-conflict".to_string(),
            operation_type: OperationType::Delete,
            version: 1,
            data: None,
        }],
        grocery_changes: vec![GroceryChangeDelta {
            id: "item-cascade-conflict".to_string(),
            operation_type: OperationType::Delete,
            version: 1,
            data: None,
        }],
        grocery_item_store_info_changes: vec![GroceryItemStoreInfoChangeDelta {
            id: "item-cascade-conflict-store-cascade-conflict".to_string(),
            grocery_item_id: "item-cascade-conflict".to_string(),
            store_id: "store-cascade-conflict".to_string(),
            operation_type: OperationType::Delete,
            version: 1,
            data: None,
        }],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res = crate::routes::sync::sync_handler(State(state.clone()), Extension(claims), AppJson(req_delete))
        .await
        .expect("Delete sync transaction should succeed even with cascade delete conflict")
        .0;

    assert!(res.success_ids.contains(&"glist-cascade-conflict".to_string()));
    assert!(res.success_ids.contains(&"cascade-conflict-member".to_string()));
    assert!(res.success_ids.contains(&"store-cascade-conflict".to_string()));
    assert!(res.success_ids.contains(&"cat-cascade-conflict".to_string()));
    assert!(res.success_ids.contains(&"item-cascade-conflict".to_string()));
    assert!(res.success_ids.contains(&"item-cascade-conflict-store-cascade-conflict".to_string()));

    // Verify all are marked as soft-deleted in the DB
    let list_db = sqlx::query!("SELECT is_deleted FROM grocery_lists WHERE id = $1", "glist-cascade-conflict")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(list_db.is_deleted);

    let member_db = sqlx::query!("SELECT is_deleted FROM grocery_list_members WHERE id = $1", "cascade-conflict-member")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(member_db.is_deleted);

    let store_db = sqlx::query!("SELECT is_deleted FROM stores WHERE id = $1", "store-cascade-conflict")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(store_db.is_deleted);

    let cat_db = sqlx::query!("SELECT is_deleted FROM categories WHERE id = $1", "cat-cascade-conflict")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(cat_db.is_deleted);

    let item_db = sqlx::query!("SELECT is_deleted FROM grocery_items WHERE id = $1", "item-cascade-conflict")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(item_db.is_deleted);

    let info_db = sqlx::query!(
        "SELECT is_deleted FROM grocery_item_store_info WHERE \"groceryItemId\" = $1 AND \"storeId\" = $2",
        "item-cascade-conflict",
        "store-cascade-conflict"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(info_db.is_deleted);
}

#[sqlx::test]
async fn test_grocery_list_delete_member_stop_collaborating(pool: PgPool) {
    let state = setup_state(pool.clone());

    // Pre-insert grocery list with owner "owner-1"
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"ownerId\", \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        "glist-stop-collab", "Collaborative List", "owner-1", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Owner member
    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "owner-member-row", "glist-stop-collab", "owner-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Collaborator member (user-2)
    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "user2-member-row", "glist-stop-collab", "user-2", "MEMBER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Associated item
    sqlx::query!(
        "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \"timesBought\", \"userId\", \"isActive\", \"listId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        "item-stop-collab", "Apples", "5", false, 0_i64, 1, 0, "owner-1", true, Some("glist-stop-collab".to_string()), 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Perform sync request as user-2 trying to delete glist-stop-collab
    let claims = Claims {
        sub: "user-2".to_string(),
        client_uuid: "client-2".to_string(),
        exp: 10000000000,
    };
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-2".to_string(),
        scope: Some(SyncScope::Grocery),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![GroceryListChangeDelta {
            id: "glist-stop-collab".to_string(),
            operation_type: OperationType::Delete,
            version: 1,
            data: None,
        }],
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

    let res = crate::routes::sync::sync_handler(State(state.clone()), Extension(claims), AppJson(req_delete))
        .await
        .expect("Stop collaborating delete action should succeed")
        .0;

    assert!(res.success_ids.contains(&"glist-stop-collab".to_string()));

    // Verify grocery list is NOT soft-deleted
    let list_db = sqlx::query!("SELECT is_deleted FROM grocery_lists WHERE id = $1", "glist-stop-collab")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!list_db.is_deleted);

    // Verify collaborator's member record IS soft-deleted
    let user2_member_db = sqlx::query!("SELECT is_deleted FROM grocery_list_members WHERE id = $1", "user2-member-row")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(user2_member_db.is_deleted);

    // Verify owner's member record is NOT soft-deleted
    let owner_member_db = sqlx::query!("SELECT is_deleted FROM grocery_list_members WHERE id = $1", "owner-member-row")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!owner_member_db.is_deleted);

    // Verify associated item is NOT soft-deleted
    let item_db = sqlx::query!("SELECT is_deleted FROM grocery_items WHERE id = $1", "item-stop-collab")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!item_db.is_deleted);
}

#[sqlx::test]
async fn test_sync_grocery_item_store_info_custom_change_id(pool: PgPool) {
    let state = setup_state(pool.clone());

    // Pre-create grocery list and store
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
        "glist-custom-id",
        "Test List Custom ID",
        0_i64,
        1_i32,
        false,
        "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "glist-custom-id-member", "glist-custom-id", "user-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO stores (id, name, position, \"isDefaultSupported\", \"userId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "store-custom-id", "Test Store Custom ID", 1, true, "user-1", 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \"timesBought\", \"userId\", \"isActive\", \"listId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        "item-custom-id", "Banana", "1", false, 0_i64, 1, 0, "user-1", true, Some("glist-custom-id".to_string()), 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    let store_info = GroceryItemStoreInfoData {
        id: "banana-store-mapping-uuid".to_string(),
        grocery_item_id: "item-custom-id".to_string(),
        store_id: "store-custom-id".to_string(),
        price: Some(0.99),
        is_available: true,
        user_id: None,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![GroceryItemStoreInfoChangeDelta {
            id: "custom-change-uuid-12345".to_string(),
            grocery_item_id: "item-custom-id".to_string(),
            store_id: "store-custom-id".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&store_info).unwrap()),
        }],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    assert!(res.success_ids.contains(&"custom-change-uuid-12345".to_string()));
    let status_found = res.upload_status.iter().find(|s| s.id == "custom-change-uuid-12345");
    assert!(status_found.is_some(), "Should find custom change ID in upload_status");
    assert_eq!(status_found.unwrap().version, 1);
}

#[sqlx::test]
async fn test_collaborator_sync_pulls_existing_items(pool: PgPool) {
    let state = setup_state(pool.clone());

    let past_time = Utc::now() - chrono::Duration::hours(1);
    let last_sync_time = Utc::now() - chrono::Duration::minutes(30);

    // Insert list
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state, updated_at, updated_by_client) VALUES ($1, $2, $3, 1, false, 'SYNCED', $4, $5)",
        "collab-list-existing", "Shared List", 0_i64, past_time, "client-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Owner member
    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state, updated_at, updated_by_client) VALUES ($1, $2, $3, $4, $5, 1, false, 'SYNCED', $6, $7)",
        "collab-owner-existing", "collab-list-existing", "user-1", "OWNER", 0_i64, past_time, "client-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Category
    sqlx::query!(
        "INSERT INTO categories (id, name, position, \"userId\", \"listId\", version, is_deleted, sync_state, updated_at, updated_by_client) VALUES ($1, $2, $3, $4, $5, 1, false, 'SYNCED', $6, $7)",
        "cat-existing", "Produce", 1, "user-1", Some("collab-list-existing".to_string()), past_time, "client-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Store
    sqlx::query!(
        "INSERT INTO stores (id, name, position, \"isDefaultSupported\", \"userId\", \"listId\", version, is_deleted, sync_state, updated_at, updated_by_client) VALUES ($1, $2, $3, $4, $5, $6, 1, false, 'SYNCED', $7, $8)",
        "store-existing", "Supermarket", 1, true, "user-1", Some("collab-list-existing".to_string()), past_time, "client-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Item
    sqlx::query!(
        "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \"timesBought\", \"userId\", \"isActive\", \"listId\", version, is_deleted, sync_state, updated_at, updated_by_client) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 1, false, 'SYNCED', $11, $12)",
        "item-existing", "Oranges", "10", false, 0_i64, 1, 0, "user-1", true, Some("collab-list-existing".to_string()), past_time, "client-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Store info
    sqlx::query!(
        "INSERT INTO grocery_item_store_info (\"groceryItemId\", \"storeId\", price, \"isAvailable\", \"userId\", version, is_deleted, sync_state, updated_at, updated_by_client) VALUES ($1, $2, $3, $4, $5, 1, false, 'SYNCED', $6, $7)",
        "item-existing", "store-existing", Some(3.49), true, "user-1", past_time, "client-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    // User-2 joins the list now
    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state, updated_at, updated_by_client) VALUES ($1, $2, $3, $4, $5, 1, false, 'SYNCED', $6, $7)",
        "collab-user2-member", "collab-list-existing", "user-2", "MEMBER", 0_i64, Utc::now(), "client-2"
    )
    .execute(&pool)
    .await
    .unwrap();

    let claims_user2 = Claims {
        sub: "user-2".to_string(),
        client_uuid: "client-2".to_string(),
        exp: 10000000000,
    };
    let req = SyncRequest {
        last_synced_at: Some(last_sync_time),
        client_id: "client-2".to_string(),
        scope: Some(SyncScope::Grocery),
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

    let res = crate::routes::sync::sync_handler(
        State(state.clone()),
        Extension(claims_user2),
        AppJson(req),
    )
    .await
    .unwrap()
    .0;

    // Verify user-2 receives the list, members, stores, categories, and items because their membership is newer than last_sync_time
    assert!(res.remote_grocery_list_changes.iter().any(|d| d.id == "collab-list-existing"));
    assert!(res.remote_grocery_list_member_changes.iter().any(|d| d.id == "collab-owner-existing"));
    assert!(res.remote_store_changes.iter().any(|d| d.id == "store-existing"));
    assert!(res.remote_category_changes.iter().any(|d| d.id == "cat-existing"));
    assert!(res.remote_grocery_changes.iter().any(|d| d.id == "item-existing"));
    assert!(res.remote_grocery_item_store_info_changes.iter().any(|d| d.grocery_item_id == "item-existing"));
}
