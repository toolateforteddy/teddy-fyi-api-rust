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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
    };

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;
    assert_eq!(res.success_ids, vec!["glist-1"]);

    // 2. Test Update. The client sends version 2; the server ignores it and moves its own
    // row on by one (DB has 1, so the row becomes 2). See `crate::routes::sync::versioning`.
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
    assert_eq!(db_row.version, 2);

    // 3. Test Delete
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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

/// Sync reflects memberships; it does not mint them, and it does not decide roles.
///
/// The row is seeded the way `/api/lists/join` seeds it, because that endpoint is the only
/// writer of `"userId"` and `role`. What sync may still do to it is bump its version and
/// delete it.
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

    // 1. An insert naming a row the server has never seen is refused, however well
    //    connected the caller is: user-1 is a member of this list, and that is still not
    //    a licence to hand membership to user-123.
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
    };

    let err = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect_err("Sync must not create a membership row");
    assert!(matches!(err, AppError::Forbidden(_)));

    let row = sqlx::query!("SELECT id FROM grocery_list_members WHERE id = $1", "member-1")
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(row.is_none(), "the refused membership must not have been written");

    // The membership as `/api/lists/join` would have written it.
    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "member-1", "glist-2", "user-123", "ADMIN", 123456_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 2. Test Update. The client sends version 2; the server ignores it and moves its own
    // row on by one (DB has 1, so the row becomes 2). See `crate::routes::sync::versioning`.
    // The `role` it sends is ignored too: roles are the server's.
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
    assert_eq!(db_row.role, "ADMIN", "the payload's role must not have been applied");
    assert_eq!(db_row.version, 2);

    // The client is told what the row actually says, so the two stop disagreeing.
    let echoed = res_update
        .remote_grocery_list_member_changes
        .iter()
        .find(|c| c.id == "member-1")
        .expect("the corrected membership row should come back");
    assert_eq!(echoed.data.as_ref().unwrap()["role"], "ADMIN");

    // 3. Test Delete
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
        list_id: None,
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
        list_id: None,
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
    };

    let err = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect_err("Handler should fail with Forbidden");

    assert!(matches!(err, AppError::Forbidden(_)));
}

/// The other half of the membership check: creating a list still works offline-first.
///
/// A client that makes a list on a plane sends the list and its own membership row in one
/// batch, and that batch must survive a rule that says "you must already be a member".
/// It does, because grocery lists are processed before members inside the same
/// transaction and creating a list seeds the creator's row — but that ordering is now
/// load-bearing, so it is pinned here rather than left as a coincidence.
#[sqlx::test]
async fn test_sync_creating_a_list_and_its_membership_in_one_batch(pool: PgPool) {
    let state = setup_state(pool.clone());

    let list_data = GroceryListData {
        id: "glist-fresh".to_string(),
        name: "Fresh List".to_string(),
        owner_id: Some("user-1".to_string()),
        created_at: 123456789,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let member_data = GroceryListMemberData {
        id: "glist-fresh-client-member".to_string(),
        list_id: "glist-fresh".to_string(),
        user_id: "user-1".to_string(),
        role: "ADMIN".to_string(),
        joined_at: 123456789,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![GroceryListChangeDelta {
            id: "glist-fresh".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&list_data).unwrap()),
        }],
        grocery_list_member_changes: vec![GroceryListMemberChangeDelta {
            id: "glist-fresh-client-member".to_string(),
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
        supports_paging: false,
    };

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("Creating a list and joining it in one batch must succeed")
        .0;
    assert!(res.success_ids.contains(&"glist-fresh-client-member".to_string()));
}

/// Knowing a listId must not be enough to join somebody else's list.
///
/// The member-change path used to authorise any insert where the row's `userId` was the
/// caller's own — "you are only adding yourself" — which is not a check: a listId travels
/// in every grocery item, store and category the list owns, so a caller who ever saw one
/// could post a membership row for it and be inside the family's data, reading and
/// writing, without guessing anything. Membership comes from `/api/lists/join` and a code,
/// and this pins that sync cannot mint it.
#[sqlx::test]
async fn test_sync_self_insert_cannot_join_a_foreign_list(pool: PgPool) {
    let state = setup_state(pool.clone());

    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
        "glist-someone-elses",
        "Their List",
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
        "glist-someone-elses-owner", "glist-someone-elses", "user-2", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // The handler's claims are user-1, who has nothing to do with this list.
    let member_data = GroceryListMemberData {
        id: "gatecrash-member".to_string(),
        list_id: "glist-someone-elses".to_string(),
        user_id: "user-1".to_string(),
        role: "MEMBER".to_string(),
        joined_at: 123456,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![GroceryListMemberChangeDelta {
            id: "gatecrash-member".to_string(),
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
        supports_paging: false,
    };

    let err = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect_err("Handler should fail with Forbidden");
    assert!(matches!(err, AppError::Forbidden(_)));

    let row = sqlx::query!(
        "SELECT id FROM grocery_list_members WHERE id = $1",
        "gatecrash-member"
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(row.is_none(), "the rejected membership must not have been written");
}

/// A membership row that already exists cannot be dragged into another list.
///
/// The upsert rewrites `"listId"` from the payload, so authorising on the *payload's* list
/// alone would let a member of their own list re-point somebody else's membership row at
/// it — or push a co-member's row out of the list they actually joined.
#[sqlx::test]
async fn test_sync_member_row_cannot_be_moved_between_lists(pool: PgPool) {
    let state = setup_state(pool.clone());

    for (list_id, member_id, user_id) in [
        ("glist-mine", "glist-mine-owner", "user-1"),
        ("glist-theirs", "glist-theirs-owner", "user-2"),
    ] {
        sqlx::query!(
            "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
            list_id,
            "List",
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
            member_id, list_id, user_id, "OWNER", 0_i64, 1_i32, false, "SYNCED"
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    // user-1 is a member of glist-mine, and names it as the target — but the row being
    // rewritten belongs to a list they have never joined.
    let member_data = GroceryListMemberData {
        id: "glist-theirs-owner".to_string(),
        list_id: "glist-mine".to_string(),
        user_id: "user-2".to_string(),
        role: "MEMBER".to_string(),
        joined_at: 0,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![GroceryListMemberChangeDelta {
            id: "glist-theirs-owner".to_string(),
            operation_type: OperationType::Update,
            version: 2,
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
        supports_paging: false,
    };

    let err = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect_err("Handler should fail with Forbidden");
    assert!(matches!(err, AppError::Forbidden(_)));

    let row = sqlx::query!(
        "SELECT \"listId\" as list_id FROM grocery_list_members WHERE id = $1",
        "glist-theirs-owner"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.list_id, "glist-theirs");
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
async fn test_sync_grocery_item_store_mapping_batch_of_shared_names(pool: PgPool) {
    // Pins the rows the store-mapping backfill produces when one payload carries several
    // new items that share a name (and one that does not). The backfill is resolved for
    // the whole batch in a single query and written with a single multi-row insert, so
    // this is the test that says the batched form still creates exactly the same rows.
    let state = setup_state(pool.clone());

    for (id, name) in [("list-alpha", "Alpha List"), ("list-beta", "Beta List")] {
        sqlx::query!(
            "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
            id, name, 0_i64, 1_i32, false, "SYNCED"
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    for (id, list_id) in [("member-alpha", "list-alpha"), ("member-beta", "list-beta")] {
        sqlx::query!(
            "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            id, list_id, "user-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    for (id, name) in [("500", "Store Omega"), ("501", "Store Sigma")] {
        sqlx::query!(
            "INSERT INTO stores (id, name, position, \"isDefaultSupported\", \"userId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            id, name, 1, true, "user-1", 1_i32, false, "SYNCED"
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    // Source items in list-alpha: "Milk" is priced in both stores, "Bread" in one.
    for (id, name) in [("600", "Milk"), ("601", "Bread")] {
        sqlx::query!(
            "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \"categoryId\", \"timesBought\", \"userId\", \"isActive\", \"listId\", unit, notes, version, is_deleted, sync_state)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
            id, name, "1", false, 0_i64, 1_i32, None::<String>, 0_i32, "user-1", true, Some("list-alpha".to_string()), None::<String>, None::<String>, 1_i32, false, "SYNCED"
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    for (item_id, store_id, price) in [("600", "500", 2.99_f64), ("600", "501", 3.49), ("601", "500", 1.50)] {
        sqlx::query!(
            "INSERT INTO grocery_item_store_info (\"groceryItemId\", \"storeId\", price, \"isAvailable\", \"userId\", version, is_deleted, sync_state, updated_by_client)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            item_id, store_id, price, true, "user-1", 1_i32, false, "SYNCED", "client-1"
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    // An item the server has already seen, named "Milk" but with no mapping of its own.
    sqlx::query!(
        "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \"categoryId\", \"timesBought\", \"userId\", \"isActive\", \"listId\", unit, notes, version, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        "710", "Milk", "1", false, 0_i64, 1_i32, None::<String>, 0_i32, "user-1", true, Some("list-beta".to_string()), None::<String>, None::<String>, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    let make_item = |id: &str, name: &str| GroceryItemData {
        id: id.to_string(),
        name: name.to_string(),
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

    let grocery_changes = vec![
        ("700", "milk", OperationType::Insert),
        ("701", "MILK", OperationType::Insert),
        ("702", "Bread", OperationType::Insert),
        ("710", "Milk", OperationType::Update),
    ]
    .into_iter()
    .map(|(id, name, op)| GroceryChangeDelta {
        id: id.to_string(),
        operation_type: op,
        version: 1,
        data: Some(serde_json::to_value(make_item(id, name)).unwrap()),
    })
    .collect();

    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes,
        grocery_item_store_info_changes: vec![],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
        supports_paging: false,
    };

    let res = sync_handler(State(state.clone()), AppJson(req))
        .await
        .expect("Sync should succeed")
        .0;

    for id in ["700", "701", "702", "710"] {
        assert!(res.success_ids.contains(&id.to_string()), "{} should sync", id);
    }

    let rows = sqlx::query!(
        "SELECT \"groceryItemId\" as grocery_item_id, \"storeId\" as store_id, price, \"isAvailable\" as is_available, \"userId\" as user_id, version, is_deleted, updated_by_client
         FROM grocery_item_store_info
         WHERE \"groceryItemId\" = ANY($1)
         ORDER BY \"groceryItemId\", \"storeId\"",
        &vec!["700".to_string(), "701".to_string(), "702".to_string(), "710".to_string()]
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    let got: Vec<(String, String, Option<f64>)> = rows
        .iter()
        .map(|r| (r.grocery_item_id.clone(), r.store_id.clone(), r.price))
        .collect();

    // Both spellings of "milk" pick up both of the source item's stores; "Bread" picks up
    // only its own. Item 710 was already known to the server, so it is not backfilled.
    assert_eq!(
        got,
        vec![
            ("700".to_string(), "500".to_string(), Some(2.99)),
            ("700".to_string(), "501".to_string(), Some(3.49)),
            ("701".to_string(), "500".to_string(), Some(2.99)),
            ("701".to_string(), "501".to_string(), Some(3.49)),
            ("702".to_string(), "500".to_string(), Some(1.50)),
        ]
    );

    for row in &rows {
        assert!(row.is_available);
        assert_eq!(row.user_id, Some("user-1".to_string()));
        assert_eq!(row.version, 1);
        assert!(!row.is_deleted);
        // MUST be NULL/None so it syncs back to client
        assert_eq!(row.updated_by_client, None);
    }
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
        product: None,
    };
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
        product: None,
    };
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
        product: None,
    };
    let req_delete = SyncRequest {
        last_synced_at: None,
        client_id: "client-2".to_string(),
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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
        list_id: None,
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
        supports_paging: false,
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
        product: None,
    };
    let req = SyncRequest {
        last_synced_at: Some(last_sync_time),
        client_id: "client-2".to_string(),
        device_uuid: None,
        device_name: None,
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
        supports_paging: false,
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

/// Helper: a list plus one membership row for `user_id`, written the way the server writes
/// them, so tests can start from a membership sync never had a hand in.
async fn seed_list_with_member(pool: &PgPool, list_id: &str, member_id: &str, user_id: &str, role: &str) {
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"ownerId\", \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (id) DO NOTHING",
        list_id,
        "List",
        Option::<String>::None,
        0_i64,
        1_i32,
        false,
        "SYNCED"
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        member_id, list_id, user_id, role, 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(pool)
    .await
    .unwrap();
}

fn member_sync_request(change: GroceryListMemberChangeDelta) -> SyncRequest {
    SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![change],
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

/// Being *in* a list is not permission to put somebody else in it.
///
/// The insert path used to write `"userId"` straight from the payload, so any member of a
/// list could hand an account membership of it with a fresh row id — no invite code, no
/// TTL, no `max_outstanding_invites_per_user`, no attempt limit. Every one of those
/// controls was optional as long as this endpoint existed.
#[sqlx::test]
async fn test_sync_member_cannot_grant_membership_to_another_account(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_list_with_member(&pool, "glist-family", "glist-family-owner", "user-1", "OWNER").await;

    let member_data = GroceryListMemberData {
        id: "smuggled-member".to_string(),
        list_id: "glist-family".to_string(),
        user_id: "user-outsider".to_string(),
        role: "MEMBER".to_string(),
        joined_at: 123456,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let err = sync_handler(
        State(state.clone()),
        AppJson(member_sync_request(GroceryListMemberChangeDelta {
            id: "smuggled-member".to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&member_data).unwrap()),
        })),
    )
    .await
    .expect_err("Sync must not grant membership");
    assert!(matches!(err, AppError::Forbidden(_)));

    let row = sqlx::query!(
        "SELECT id FROM grocery_list_members WHERE \"userId\" = $1",
        "user-outsider"
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(row.is_none(), "no membership may exist for an account that never joined");
}

/// An existing row's `"userId"` is the server's; a payload that disagrees is refused
/// outright rather than quietly applied.
#[sqlx::test]
async fn test_sync_member_update_cannot_reassign_user_id(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_list_with_member(&pool, "glist-family", "glist-family-owner", "user-1", "OWNER").await;

    let member_data = GroceryListMemberData {
        id: "glist-family-owner".to_string(),
        list_id: "glist-family".to_string(),
        user_id: "user-outsider".to_string(),
        role: "OWNER".to_string(),
        joined_at: 0,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let err = sync_handler(
        State(state.clone()),
        AppJson(member_sync_request(GroceryListMemberChangeDelta {
            id: "glist-family-owner".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&member_data).unwrap()),
        })),
    )
    .await
    .expect_err("Sync must not reassign a membership");
    assert!(matches!(err, AppError::Forbidden(_)));

    let row = sqlx::query!(
        "SELECT \"userId\" as user_id FROM grocery_list_members WHERE id = $1",
        "glist-family-owner"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.user_id, "user-1");
}

/// A membership given up comes back through `/api/lists/join`, not through a client
/// syncing `isDeleted: false` over the top of the row it left behind.
#[sqlx::test]
async fn test_sync_member_cannot_resurrect_a_deleted_membership(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_list_with_member(&pool, "glist-family", "glist-family-owner", "user-2", "OWNER").await;

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "glist-family-ex", "glist-family", "user-1", "MEMBER", 0_i64, 1_i32, true, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    let member_data = GroceryListMemberData {
        id: "glist-family-ex".to_string(),
        list_id: "glist-family".to_string(),
        user_id: "user-1".to_string(),
        role: "MEMBER".to_string(),
        joined_at: 0,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let err = sync_handler(
        State(state.clone()),
        AppJson(member_sync_request(GroceryListMemberChangeDelta {
            id: "glist-family-ex".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&member_data).unwrap()),
        })),
    )
    .await
    .expect_err("Sync must not restore a membership");
    assert!(matches!(err, AppError::Forbidden(_)));

    let row = sqlx::query!(
        "SELECT is_deleted FROM grocery_list_members WHERE id = $1",
        "glist-family-ex"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(row.is_deleted, "the membership must stay gone");
}

/// `role` is a capability, not a field: a member who writes `OWNER` into their own row
/// must not gain the power to delete the list and everything on it.
///
/// `grocery_lists` reads `role == "OWNER"` to authorise a list delete, which soft-deletes
/// every item, store and category on it — for the whole family. This test runs both halves:
/// the promotion is ignored, and the delete it was for is still refused.
#[sqlx::test]
async fn test_sync_member_cannot_promote_self_to_owner(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_list_with_member(&pool, "glist-shared", "glist-shared-owner", "user-2", "OWNER").await;

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "glist-shared-guest", "glist-shared", "user-1", "MEMBER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    let member_data = GroceryListMemberData {
        id: "glist-shared-guest".to_string(),
        list_id: "glist-shared".to_string(),
        user_id: "user-1".to_string(),
        role: "OWNER".to_string(),
        joined_at: 0,
        version: 2,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let _ = sync_handler(
        State(state.clone()),
        AppJson(member_sync_request(GroceryListMemberChangeDelta {
            id: "glist-shared-guest".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: Some(serde_json::to_value(&member_data).unwrap()),
        })),
    )
    .await
    .expect("a version bump on your own membership row is still allowed");

    let row = sqlx::query!(
        "SELECT role FROM grocery_list_members WHERE id = $1",
        "glist-shared-guest"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.role, "MEMBER", "roles are server-assigned");

    // And the delete the promotion existed for is refused.
    let mut delete_req = member_sync_request(GroceryListMemberChangeDelta {
        id: "unused".to_string(),
        operation_type: OperationType::Update,
        version: 1,
        data: None,
    });
    delete_req.grocery_list_member_changes = vec![];
    delete_req.grocery_list_changes = vec![GroceryListChangeDelta {
        id: "glist-shared".to_string(),
        operation_type: OperationType::Delete,
        version: 2,
        data: None,
    }];

    let _ = sync_handler(State(state.clone()), AppJson(delete_req))
        .await
        .expect("a non-owner delete is answered, not fatal");

    let list = sqlx::query!(
        "SELECT is_deleted FROM grocery_lists WHERE id = $1",
        "glist-shared"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!list.is_deleted, "a member must not be able to delete a shared list");
}

/// A member who joins an existing list receives the items already in it.
///
/// This is what the `glm.updated_at > cursor` disjunct in every grocery download query
/// buys, and it is not obvious from reading them: the disjunct means *any* change to the
/// membership row re-sends the whole list, which reads like an accident and costs real
/// bandwidth on a shared list. It is load-bearing. A joining member's cursor is their own
/// account's last sync, which is newer than items the list has held for weeks, so without
/// the disjunct `gi.updated_at > cursor` matches none of them and the list arrives empty
/// and stays empty until somebody edits each item.
///
/// The cost is real and so is the reason. Anyone narrowing this — and it is worth
/// narrowing, since a role change re-sends a list for no reason — has to keep this case
/// working.
#[sqlx::test]
async fn test_new_member_receives_items_older_than_their_cursor(pool: PgPool) {
    let state = setup_state(pool.clone());

    // A list with an item on it, both stamped well in the past.
    seed_list_with_member(&pool, "list-old", "member-owner", "user-owner", "OWNER").await;
    sqlx::query!(
        "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \
         \"timesBought\", \"userId\", \"isActive\", \"listId\", version, is_deleted, sync_state, updated_at) \
         VALUES ($1, $2, '1', FALSE, 0, 0, 0, $3, TRUE, $4, 1, FALSE, 'SYNCED', now() - interval '30 days')",
        "item-old",
        "Milk",
        "user-owner",
        "list-old"
    )
    .execute(&pool)
    .await
    .unwrap();

    // user-1 joins today. Their membership row is new; the item is thirty days old.
    seed_list_with_member(&pool, "list-old", "member-new", "user-1", "MEMBER").await;

    // ...and syncs with a cursor from an hour ago, long after the item was written.
    let mut req = member_sync_request(GroceryListMemberChangeDelta {
        id: "unused".to_string(),
        operation_type: OperationType::Update,
        version: 1,
        data: None,
    });
    // A pure download: no uploads at all, just a cursor from after the item was written.
    req.grocery_list_member_changes = vec![];
    req.last_synced_at = Some(Utc::now() - chrono::Duration::hours(1));

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("sync must succeed")
        .0;

    let ids: Vec<&str> = res
        .remote_grocery_changes
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert!(
        ids.contains(&"item-old"),
        "a newly joined member must receive the items already on the list; got {ids:?}"
    );
}
