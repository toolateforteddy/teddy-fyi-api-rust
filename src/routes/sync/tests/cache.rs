use sqlx::PgPool;
use axum::extract::State;
use axum::Extension;
use chrono::Utc;
use redis::AsyncCommands;
use crate::routes::sync::tests::helpers::{setup_state, sync_handler};
use crate::routes::sync::{
    SyncRequest, SyncScope, TodoListData, TodoListChangeDelta, GroceryItemData, GroceryChangeDelta,
    OperationType, AppJson, SyncStatusQuery, sync_status_handler
};
use crate::auth::tokens::Claims;

#[sqlx::test]
async fn test_sync_status_handler_db_fallback(pool: PgPool) {
    let state = setup_state(pool.clone());
    let test_user = "user-status-db-fallback";
    
    // Clear any existing cache for test_user
    if let Ok(mut conn) = state.redis_client.get_multiplexed_tokio_connection().await {
        let _: i32 = conn.del(&format!("user:{}:last_update:All", test_user)).await.unwrap_or(0);
    }

    // Insert a todo list for test_user
    // Use RETURNING updated_at to get the exact database-assigned timestamp
    let row = sqlx::query!(
        r#"INSERT INTO todo_lists (id, name, "colorHex", "userId", "createdAt", sync_state, version, is_deleted, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
         RETURNING updated_at"#,
        "todolist-status-1",
        "Status List",
        "#FF0000",
        test_user,
        0_i64,
        "SYNCED",
        1_i32,
        false,
        "client-1"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    let updated_time = row.updated_at;

    let claims = Claims {
        sub: test_user.to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10000000000,
    };

    // Case 1: client last_synced_at is older (needs sync)
    let query_older = SyncStatusQuery {
        last_synced_at: Some(updated_time - chrono::Duration::minutes(5)),
        scope: Some(SyncScope::All),
    };
    let res = sync_status_handler(State(state.clone()), Extension(claims.clone()), axum::extract::Query(query_older))
        .await
        .expect("Status handler should succeed")
        .0;

    assert!(res.needs_sync);
    assert_eq!(res.latest_version, updated_time);

    // Case 2: client last_synced_at is newer (does not need sync)
    let query_newer = SyncStatusQuery {
        last_synced_at: Some(updated_time + chrono::Duration::minutes(5)),
        scope: Some(SyncScope::All),
    };
    let res_newer = sync_status_handler(State(state.clone()), Extension(claims.clone()), axum::extract::Query(query_newer))
        .await
        .expect("Status handler should succeed")
        .0;

    assert!(!res_newer.needs_sync);

    // Verify key was set in Redis
    if let Ok(mut conn) = state.redis_client.get_multiplexed_tokio_connection().await {
        let ts_str: Option<String> = conn.get(&format!("user:{}:last_update:All", test_user)).await.unwrap_or(None);
        assert!(ts_str.is_some());
        
        // Clean up
        let _: i32 = conn.del(&format!("user:{}:last_update:All", test_user)).await.unwrap_or(0);
    }
}

#[sqlx::test]
async fn test_sync_status_handler_cache_hit(pool: PgPool) {
    let state = setup_state(pool.clone());
    let test_user = "user-status-cache-hit";
    
    // Only run if Redis is actually connectable
    if let Ok(mut conn) = state.redis_client.get_multiplexed_tokio_connection().await {
        let cached_time = Utc::now() - chrono::Duration::hours(2);
        let ts_str = cached_time.to_rfc3339();
        
        let _: () = conn.set(&format!("user:{}:last_update:Todo", test_user), &ts_str).await.unwrap();

        let claims = Claims {
            sub: test_user.to_string(),
            client_uuid: "client-1".to_string(),
            exp: 10000000000,
        };

        // Even if DB has a newer/older timestamp or is empty, it should use the cache
        let query = SyncStatusQuery {
            last_synced_at: Some(cached_time - chrono::Duration::minutes(5)),
            scope: Some(SyncScope::Todo),
        };
        let res = sync_status_handler(State(state.clone()), Extension(claims.clone()), axum::extract::Query(query))
            .await
            .expect("Status handler should succeed")
            .0;

        assert!(res.needs_sync);
        assert!((res.latest_version - cached_time).num_seconds().abs() < 2);

        // Clean up
        let _: i32 = conn.del(&format!("user:{}:last_update:Todo", test_user)).await.unwrap_or(0);
    }
}

#[sqlx::test]
async fn test_sync_handler_updates_redis_cache(pool: PgPool) {
    let state = setup_state(pool.clone());
    let test_user = "user-updates-redis-cache";

    if let Ok(mut conn) = state.redis_client.get_multiplexed_tokio_connection().await {
        let _: i32 = conn.del(&format!("user:{}:last_update:All", test_user)).await.unwrap_or(0);
        let _: i32 = conn.del(&format!("user:{}:last_update:Todo", test_user)).await.unwrap_or(0);
        let _: i32 = conn.del(&format!("user:{}:last_update:Grocery", test_user)).await.unwrap_or(0);
    }

    let list_data = TodoListData {
        id: "list-status-cache-1".to_string(),
        name: "Cache Test List".to_string(),
        color_hex: "#FF0000".to_string(),
        user_id: Some(test_user.to_string()),
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
            id: "list-status-cache-1".to_string(),
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

    let claims = Claims {
        sub: test_user.to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10000000000,
    };

    let res = crate::routes::sync::sync_handler(State(state.clone()), Extension(claims.clone()), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    assert_eq!(res.success_ids, vec!["list-status-cache-1"]);

    // Verify Redis has keys updated for All and Todo
    if let Ok(mut conn) = state.redis_client.get_multiplexed_tokio_connection().await {
        let all_ts: Option<String> = conn.get(&format!("user:{}:last_update:All", test_user)).await.unwrap_or(None);
        let todo_ts: Option<String> = conn.get(&format!("user:{}:last_update:Todo", test_user)).await.unwrap_or(None);
        let grocery_ts: Option<String> = conn.get(&format!("user:{}:last_update:Grocery", test_user)).await.unwrap_or(None);

        assert!(all_ts.is_some(), "All cache key should be updated");
        assert!(todo_ts.is_some(), "Todo cache key should be updated");
        assert!(grocery_ts.is_none(), "Grocery cache key should not be updated");

        // Clean up
        let _: i32 = conn.del(&format!("user:{}:last_update:All", test_user)).await.unwrap_or(0);
        let _: i32 = conn.del(&format!("user:{}:last_update:Todo", test_user)).await.unwrap_or(0);
    }
}

#[sqlx::test]
async fn test_sync_handler_updates_redis_cache_collaborative(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_a = "user-A";
    let user_b = "user-B";
    let list_id = "grocerylist-collab-1";

    // Setup list and members in Postgres sandbox
    sqlx::query!(
        r#"INSERT INTO grocery_lists (id, name, "ownerId", "createdAt", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, NOW(), $6, $7, $8)"#,
        list_id,
        "Collab List",
        user_a,
        0_i64,
        1_i32,
        "client-a",
        false,
        "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())",
        "member-a-1", list_id, user_a, "OWNER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO grocery_list_members (id, \"listId\", \"userId\", role, \"joinedAt\", version, is_deleted, sync_state, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW())",
        "member-b-1", list_id, user_b, "MEMBER", 0_i64, 1_i32, false, "SYNCED"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Clear any existing cache for user_a and user_b
    if let Ok(mut conn) = state.redis_client.get_multiplexed_tokio_connection().await {
        let _: i32 = conn.del(&format!("user:{}:last_update:All", user_a)).await.unwrap_or(0);
        let _: i32 = conn.del(&format!("user:{}:last_update:Grocery", user_a)).await.unwrap_or(0);
        let _: i32 = conn.del(&format!("user:{}:last_update:All", user_b)).await.unwrap_or(0);
        let _: i32 = conn.del(&format!("user:{}:last_update:Grocery", user_b)).await.unwrap_or(0);
    }

    let item_data = GroceryItemData {
        id: "grocery-item-collab-1".to_string(),
        name: "Apples".to_string(),
        quantity: "5".to_string(),
        is_bought: false,
        created_at: 0,
        position: 1,
        category_id: None,
        times_bought: 0,
        user_id: None,
        is_active: true,
        list_id: Some(list_id.to_string()),
        unit: None,
        notes: None,
        version: 1,
        is_deleted: false,
        sync_state: "SYNCED".to_string(),
    };

    let req = SyncRequest {
        last_synced_at: None,
        client_id: "client-a".to_string(),
        scope: Some(SyncScope::Grocery),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![GroceryChangeDelta {
            id: "grocery-item-collab-1".to_string(),
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

    let claims = Claims {
        sub: user_a.to_string(),
        client_uuid: "client-a".to_string(),
        exp: 10000000000,
    };

    let res = crate::routes::sync::sync_handler(State(state.clone()), Extension(claims.clone()), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    assert_eq!(res.success_ids, vec!["grocery-item-collab-1"]);

    // Verify Redis has keys updated for both user_a and user_b
    if let Ok(mut conn) = state.redis_client.get_multiplexed_tokio_connection().await {
        let all_ts_a: Option<String> = conn.get(&format!("user:{}:last_update:All", user_a)).await.unwrap_or(None);
        let grocery_ts_a: Option<String> = conn.get(&format!("user:{}:last_update:Grocery", user_a)).await.unwrap_or(None);
        
        let all_ts_b: Option<String> = conn.get(&format!("user:{}:last_update:All", user_b)).await.unwrap_or(None);
        let grocery_ts_b: Option<String> = conn.get(&format!("user:{}:last_update:Grocery", user_b)).await.unwrap_or(None);

        assert!(all_ts_a.is_some(), "User A All cache key should be updated");
        assert!(grocery_ts_a.is_some(), "User A Grocery cache key should be updated");
        assert!(all_ts_b.is_some(), "User B All cache key should be updated");
        assert!(grocery_ts_b.is_some(), "User B Grocery cache key should be updated");

        // Clean up
        let _: i32 = conn.del(&format!("user:{}:last_update:All", user_a)).await.unwrap_or(0);
        let _: i32 = conn.del(&format!("user:{}:last_update:Grocery", user_a)).await.unwrap_or(0);
        let _: i32 = conn.del(&format!("user:{}:last_update:All", user_b)).await.unwrap_or(0);
        let _: i32 = conn.del(&format!("user:{}:last_update:Grocery", user_b)).await.unwrap_or(0);
    }
}
