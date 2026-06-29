use sqlx::PgPool;
use axum::extract::State;
use axum::{Extension, Json};
use chrono::Utc;
use crate::routes::sync::tests::helpers::setup_state;
use crate::routes::sync::{
    SyncRequest, SyncScope, GroceryListChangeDelta, StoreChangeDelta,
    GroceryChangeDelta, OperationType, AppJson
};
use crate::auth::tokens::Claims;

#[sqlx::test]
async fn test_login_upserts_user(pool: PgPool) {
    let mut state = setup_state(pool.clone());
    state.cookie_domain = "".to_string(); // bypass Google OAuth validation via dev/mock token

    let payload = crate::auth::handlers::LoginRequest {
        user_id: "user-test-login-upsert".to_string(),
        client_uuid: "client-upsert".to_string(),
        google_auth_token: "mock.token".to_string(),
        use_cookie: Some(false),
    };

    let response = crate::auth::handlers::login_handler(State(state.clone()), Json(payload))
        .await
        .expect("Login should succeed");

    assert_eq!(response.status(), axum::http::StatusCode::OK);

    // Verify user exists in the database
    let user = sqlx::query!(
        "SELECT email FROM users WHERE id = $1",
        "user-test-login-upsert"
    )
    .fetch_one(&pool)
    .await
    .expect("User should have been upserted");
    assert_eq!(user.email, Some("dev-user@teddy.fyi".to_string()));
}

#[sqlx::test]
async fn test_invite_system_flow(pool: PgPool) {
    let state = setup_state(pool.clone());
    
    // 1. Create a list owned by user-1
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
        "shared-list-1", "Home List", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "shared-list-1-owner", "shared-list-1", "user-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 2. Generate invite code as user-1
    let claims_user1 = Claims {
        sub: "user-1".to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10000000000,
    };
    
    let invite_res = crate::routes::lists::handlers::invite_handler(
        State(state.clone()),
        Extension(claims_user1),
        axum::Json(crate::routes::lists::handlers::InviteRequest {
            list_id: "shared-list-1".to_string(),
        }),
    )
    .await
    .expect("Invite generation should succeed")
    .0;

    let code = invite_res.code;
    assert_eq!(code.len(), 8);

    // Verify invite exists in DB
    let invite_db = sqlx::query!("SELECT code, \"listId\" as list_id FROM list_invites WHERE code = $1", code)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(invite_db.list_id, "shared-list-1");

    // 3. User-2 joins the list using the invite code
    let claims_user2 = Claims {
        sub: "user-2".to_string(),
        client_uuid: "client-2".to_string(),
        exp: 10000000000,
    };

    let join_res = crate::routes::lists::handlers::join_handler(
        State(state.clone()),
        Extension(claims_user2),
        axum::Json(crate::routes::lists::handlers::JoinRequest {
            code: code.clone(),
        }),
    )
    .await
    .expect("Joining list should succeed")
    .0;

    assert!(join_res.success);
    assert_eq!(join_res.list_id, "shared-list-1");

    // Verify User-2 is now in members table
    let member_db = sqlx::query!(
        "SELECT role, is_deleted FROM grocery_list_members WHERE \"listId\" = $1 AND \"userId\" = $2",
        "shared-list-1", "user-2"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(member_db.role, "MEMBER");
    assert!(!member_db.is_deleted);

    // Verify invite code is deleted (single-use)
    let invite_after = sqlx::query!("SELECT 1 as dummy FROM list_invites WHERE code = $1", code)
        .fetch_optional(&pool)
        .await
        .unwrap();
    assert!(invite_after.is_none());
}

#[sqlx::test]
async fn test_sync_collaborative_scoping(pool: PgPool) {
    let state = setup_state(pool.clone());

    // Pre-insert a shared list, store, category, and grocery item
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"createdAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6)",
        "collab-list", "Shared List", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "collab-owner", "collab-list", "user-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Store tied to list
    sqlx::query!(
        "INSERT INTO stores (id, name, position, \"isDefaultSupported\", \"userId\", \"listId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        "500", "Shared Store", 1, true, "user-1", Some("collab-list".to_string()), 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Category tied to list
    sqlx::query!(
        "INSERT INTO categories (id, name, position, \"userId\", \"listId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "600", "Shared Category", 1, "user-1", Some("collab-list".to_string()), 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Grocery Item tied to list
    sqlx::query!(
        "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \"timesBought\", \"userId\", \"isActive\", \"listId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        "700", "Shared Apples", "5", false, 0_i64, 1, 0, "user-1", true, Some("collab-list".to_string()), 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Grocery item store info mapping
    sqlx::query!(
        "INSERT INTO grocery_item_store_info (\"groceryItemId\", \"storeId\", price, \"isAvailable\", \"userId\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, 1, false, 'SYNCED')",
        "700", "500", Some(2.99), true, "user-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 1. Sync as User-2 (not a member of collab-list yet)
    let claims_user2 = Claims {
        sub: "user-2".to_string(),
        client_uuid: "client-2".to_string(),
        exp: 10000000000,
    };
    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
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

    let res_not_member = crate::routes::sync::sync_handler(
        State(state.clone()),
        Extension(claims_user2.clone()),
        AppJson(req.clone()),
    )
    .await
    .unwrap()
    .0;

    // Verify User-2 gets NO changes (they aren't a member)
    assert!(res_not_member.remote_store_changes.is_empty());
    assert!(res_not_member.remote_category_changes.is_empty());
    assert!(res_not_member.remote_grocery_changes.is_empty());
    assert!(res_not_member.remote_grocery_item_store_info_changes.is_empty());

    // 2. Add User-2 as a member
    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "collab-member", "collab-list", "user-2", "MEMBER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 3. Sync again as User-2
    let res_member = crate::routes::sync::sync_handler(
        State(state.clone()),
        Extension(claims_user2),
        AppJson(req),
    )
    .await
    .unwrap()
    .0;

    // Verify User-2 now receives the collaborative stores, categories, and items
    assert_eq!(res_member.remote_store_changes.len(), 1);
    assert_eq!(res_member.remote_store_changes[0].id, "500");

    assert_eq!(res_member.remote_category_changes.len(), 1);
    assert_eq!(res_member.remote_category_changes[0].id, "600");

    assert_eq!(res_member.remote_grocery_changes.len(), 1);
    assert_eq!(res_member.remote_grocery_changes[0].id, "700");

    assert_eq!(res_member.remote_grocery_item_store_info_changes.len(), 1);
    assert_eq!(res_member.remote_grocery_item_store_info_changes[0].id, "700-500");
    assert_eq!(res_member.remote_grocery_item_store_info_changes[0].grocery_item_id, "700");
    assert_eq!(res_member.remote_grocery_item_store_info_changes[0].store_id, "500");
}

#[sqlx::test]
async fn test_sync_handler_need_update_state_recovery(pool: PgPool) {
    // 1. Setup grocery list, store, and item
    sqlx::query!(
        "INSERT INTO grocery_lists (id, name, \"ownerId\", \"createdAt\", version, is_deleted, sync_state, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "need-list-1", "Fetch List", Some("user-1"), 0_i64, 1_i32, false, "SYNCED", "client-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        "need-member-1", "need-list-1", "user-1", "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO stores (id, name, position, \"isDefaultSupported\", \"userId\", version, is_deleted, sync_state, \"listId\", updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        "need-store-1", "Fetch Store", 1_i32, false, Some("user-1"), 1_i32, false, "SYNCED", Some("need-list-1"), "client-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_items (id, name, quantity, \"isBought\", \"createdAt\", position, \"timesBought\", \"userId\", \"isActive\", \"listId\", unit, notes, version, is_deleted, sync_state, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
        "need-item-1", "Fetch Item", "1", false, 0_i64, 1_i32, 0_i32, Some("user-1"), true, Some("need-list-1"), None::<String>, None::<String>, 1_i32, false, "SYNCED", "client-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = setup_state(pool.clone());
    let claims = Claims {
        sub: "user-1".to_string(),
        client_uuid: "client-2".to_string(),
        exp: 10000000000,
    };

    // Client requests state recovery for all 3 using UPDATE with data: null/None
    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-2".to_string(),
        scope: Some(SyncScope::Grocery),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![GroceryListChangeDelta {
            id: "need-list-1".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: None,
        }],
        grocery_list_member_changes: vec![],
        store_changes: vec![StoreChangeDelta {
            id: "need-store-1".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: None,
        }],
        category_changes: vec![],
        grocery_changes: vec![GroceryChangeDelta {
            id: "need-item-1".to_string(),
            operation_type: OperationType::Update,
            version: 2,
            data: None,
        }],
        grocery_item_store_info_changes: vec![],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res = crate::routes::sync::sync_handler(
        State(state.clone()),
        Extension(claims),
        AppJson(req),
    )
    .await
    .unwrap()
    .0;

    // Verify all 3 succeeded
    assert!(res.success_ids.contains(&"need-list-1".to_string()));
    assert!(res.success_ids.contains(&"need-store-1".to_string()));
    assert!(res.success_ids.contains(&"need-item-1".to_string()));

    // Verify database was NOT modified (No-Op on Write)
    let db_list = sqlx::query!("SELECT version, updated_by_client FROM grocery_lists WHERE id = 'need-list-1'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(db_list.version, 1);
    assert_eq!(db_list.updated_by_client, Some("client-1".to_string()));

    let db_store = sqlx::query!("SELECT version, updated_by_client FROM stores WHERE id = 'need-store-1'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(db_store.version, 1);
    assert_eq!(db_store.updated_by_client, Some("client-1".to_string()));

    let db_item = sqlx::query!("SELECT version, updated_by_client FROM grocery_items WHERE id = 'need-item-1'")
        .fetch_one(&pool).await.unwrap();
    assert_eq!(db_item.version, 1);
    assert_eq!(db_item.updated_by_client, Some("client-1".to_string()));

    // Verify Force Download: remote changes must return the full records with current version (1)
    let remote_list = res.remote_grocery_list_changes.iter().find(|d| d.id == "need-list-1").unwrap();
    assert_eq!(remote_list.version, 1);
    let list_data: crate::routes::sync::GroceryListData = serde_json::from_value(remote_list.data.as_ref().unwrap().clone()).unwrap();
    assert_eq!(list_data.name, "Fetch List");

    let remote_store = res.remote_store_changes.iter().find(|d| d.id == "need-store-1").unwrap();
    assert_eq!(remote_store.version, 1);
    let store_data: crate::routes::sync::StoreData = serde_json::from_value(remote_store.data.as_ref().unwrap().clone()).unwrap();
    assert_eq!(store_data.name, "Fetch Store");

    let remote_item = res.remote_grocery_changes.iter().find(|d| d.id == "need-item-1").unwrap();
    assert_eq!(remote_item.version, 1);
    let item_data: crate::routes::sync::GroceryItemData = serde_json::from_value(remote_item.data.as_ref().unwrap().clone()).unwrap();
    assert_eq!(item_data.name, "Fetch Item");
}
