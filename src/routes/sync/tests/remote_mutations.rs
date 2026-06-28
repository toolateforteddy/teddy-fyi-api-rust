use sqlx::PgPool;
use axum::extract::State;
use chrono::Utc;
use crate::routes::sync::tests::helpers::{setup_state, sync_handler};
use crate::routes::sync::{
    SyncRequest, SyncScope, AppJson, fetch_remote_todo_mutations, fetch_remote_grocery_mutations,
    parse_or_hash_uuid
};

#[sqlx::test]
async fn test_sync_handler_remote_mutations(pool: PgPool) {
    // Insert an old record (not fetched)
    sqlx::query!(
        "INSERT INTO todo_items (id, title, \"isCompleted\", \"createdAt\", position, \"scheduledAt\", \"userId\", \"isDaily\", priority, icon, sync_state, version, is_deleted, updated_at, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NOW() - INTERVAL '1 hour', $14)",
        "todo-old", "Old", false, 0_i64, 0_i32, 0_i64, "user-1", false, 0_i32, None::<String>, "SYNCED", 1_i32, false, "client-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    // Insert a new record (should be fetched)
    sqlx::query!(
        "INSERT INTO todo_items (id, title, \"isCompleted\", \"createdAt\", position, \"scheduledAt\", \"userId\", \"isDaily\", priority, icon, sync_state, version, is_deleted, updated_at, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NOW(), $14)",
        "todo-new", "New", false, 0_i64, 0_i32, 0_i64, "user-1", false, 0_i32, None::<String>, "SYNCED", 2_i32, false, "client-1"
    )
    .execute(&pool)
    .await
    .unwrap();

    let state = setup_state(pool.clone());
    let last_synced = Utc::now() - chrono::Duration::minutes(30);

    let req = SyncRequest {
        last_synced_at: Some(last_synced),
        client_id: "client-2".to_string(),
        scope: None, // different client id, so it gets the changes
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

    // Should only fetch the "todo-new" since "todo-old" is older than 30 mins
    assert_eq!(res.remote_todo_changes.len(), 1);
    assert_eq!(res.remote_todo_changes[0].id, "todo-new");
    assert_eq!(res.remote_todo_changes[0].version, 2);
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
        r#"INSERT INTO todo_items (id, title, "isCompleted", "createdAt", position, "scheduledAt", "userId", "isDaily", priority, icon, sync_state, version, is_deleted, updated_at, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NOW(), $14)"#,
        "todoitem-remote-1",
        "Remote Todo",
        false,
        0_i64,
        1_i32,
        0_i64,
        "user-1",
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
    let (todo_lists, todo_items) = fetch_remote_todo_mutations(&mut tx, "user-1", client_id, last_synced_at)
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
        "1001",
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
        "2001",
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
        "3001",
        "Remote Grocery Item",
        "1",
        false,
        0_i64,
        1_i32,
        Some("2001".to_string()),
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
        "3001",
        "1001",
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
    ) = fetch_remote_grocery_mutations(&mut tx, "user-1", client_id, last_synced_at)
        .await
        .unwrap();

    assert!(grocery_lists.iter().any(|d| d.id == "grocerylist-remote-1"));
    assert!(grocery_list_members
        .iter()
        .any(|d| d.id == "member-remote-1"));
    assert!(stores.iter().any(|d| d.id == "1001"));
    assert!(categories.iter().any(|d| d.id == "2001"));
    assert!(grocery_items.iter().any(|d| d.id == "3001"));
    assert!(grocery_item_store_infos
        .iter()
        .any(|d| d.id == "3001-1001" && d.grocery_item_id == "3001" && d.store_id == "1001"));

    tx.rollback().await.unwrap();
}

#[sqlx::test]
async fn test_fetch_remote_mutations_initial_sync_none(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();

    let client_id = "test-client";
    let last_synced_at = None;

    // --- todo_lists ---
    sqlx::query!(
        r#"INSERT INTO todo_lists (id, name, "colorHex", "userId", "createdAt", sync_state, version, is_deleted, updated_at, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), $9)"#,
        "todolist-initial-1",
        "Initial List",
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

    // Verify TODO mutations with last_synced_at = None
    let (todo_lists, _) = fetch_remote_todo_mutations(&mut tx, "user-1", client_id, last_synced_at)
        .await
        .unwrap();

    assert!(todo_lists.iter().any(|d| d.id == "todolist-initial-1"));

    // --- grocery_lists ---
    sqlx::query!(
        r#"INSERT INTO grocery_lists (id, name, "ownerId", "createdAt", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, NOW(), $6, $7, $8)"#,
        "grocerylist-initial-1",
        "Initial Grocery List",
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

    // --- grocery_list_members ---
    sqlx::query!(
        r#"INSERT INTO grocery_list_members (id, "listId", "userId", role, "joinedAt", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, NOW(), $7, $8, $9)"#,
        "member-initial-1",
        "grocerylist-initial-1",
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

    // Verify Grocery mutations with last_synced_at = None
    let (
        grocery_lists,
        grocery_list_members,
        _,
        _,
        _,
        _,
    ) = fetch_remote_grocery_mutations(&mut tx, "user-1", client_id, last_synced_at)
        .await
        .unwrap();

    assert!(grocery_lists.iter().any(|d| d.id == "grocerylist-initial-1"));
    assert!(grocery_list_members.iter().any(|d| d.id == "member-initial-1"));

    tx.rollback().await.unwrap();
}

#[sqlx::test]
async fn test_fetch_remote_mutations_initial_sync_epoch(pool: PgPool) {
    let mut tx = pool.begin().await.unwrap();

    let client_id = "test-client";
    let last_synced_at = Some(chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap());

    // --- todo_lists ---
    sqlx::query!(
        r#"INSERT INTO todo_lists (id, name, "colorHex", "userId", "createdAt", sync_state, version, is_deleted, updated_at, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), $9)"#,
        "todolist-epoch-1",
        "Epoch List",
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

    // Verify TODO mutations with last_synced_at = Epoch
    let (todo_lists, _) = fetch_remote_todo_mutations(&mut tx, "user-1", client_id, last_synced_at)
        .await
        .unwrap();

    assert!(todo_lists.iter().any(|d| d.id == "todolist-epoch-1"));

    // --- grocery_lists ---
    sqlx::query!(
        r#"INSERT INTO grocery_lists (id, name, "ownerId", "createdAt", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, NOW(), $6, $7, $8)"#,
        "grocerylist-epoch-1",
        "Epoch Grocery List",
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

    // --- grocery_list_members ---
    sqlx::query!(
        r#"INSERT INTO grocery_list_members (id, "listId", "userId", role, "joinedAt", version, updated_at, updated_by_client, is_deleted, sync_state)
         VALUES ($1, $2, $3, $4, $5, $6, NOW(), $7, $8, $9)"#,
        "member-epoch-1",
        "grocerylist-epoch-1",
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

    // Verify Grocery mutations with last_synced_at = Epoch
    let (
        grocery_lists,
        _,
        _,
        _,
        _,
        _,
    ) = fetch_remote_grocery_mutations(&mut tx, "user-1", client_id, last_synced_at)
        .await
        .unwrap();

    assert!(grocery_lists.iter().any(|d| d.id == "grocerylist-epoch-1"));

    tx.rollback().await.unwrap();
}

#[sqlx::test]
async fn test_sync_handler_epoch_initial_sync_bypasses_echo(pool: PgPool) {
    let state = setup_state(pool.clone());
    let client_id = "client-1";
    let client_uuid = parse_or_hash_uuid(client_id);
    let user_uuid = parse_or_hash_uuid("user-1");

    // 1. Insert a config updated by client-1
    sqlx::query!(
        "INSERT INTO configs (id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, key, value) \
         VALUES ($1, $2, $3, $4, $5, $6, 'SYNCED'::sync_state, $7, $8)",
        uuid::Uuid::new_v4(),
        user_uuid,
        client_uuid,
        1_i32,
        false,
        Utc::now().timestamp_millis(),
        "theme",
        "dark"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 2. Prepare request with last_synced_at = Epoch
    let req = SyncRequest {
        last_synced_at: Some(chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap()),
        client_id: client_id.to_string(),
        scope: Some(SyncScope::ScribbleKeep),
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

    // Verify config is returned despite being updated by client-1
    assert_eq!(res.configs.len(), 1);
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
        r#"INSERT INTO todo_items (id, title, "isCompleted", "createdAt", position, "scheduledAt", "userId", "isDaily", priority, icon, sync_state, version, is_deleted, updated_at, updated_by_client)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, NOW(), $14)"#,
        "todoitem-echo-1",
        "Echo Todo",
        false,
        0_i64,
        1_i32,
        0_i64,
        "user-1",
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
    let (todo_lists, todo_items) = fetch_remote_todo_mutations(&mut tx, "user-1", client_id, last_synced_at)
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
        "1002",
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
        "2002",
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
        "3002",
        "Echo Grocery Item",
        "1",
        false,
        0_i64,
        1_i32,
        Some("2002".to_string()),
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
        "3002",
        "1002",
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
    ) = fetch_remote_grocery_mutations(&mut tx, "user-1", client_id, last_synced_at)
        .await
        .unwrap();

    assert!(!grocery_lists.iter().any(|d| d.id == "grocerylist-echo-1"));
    assert!(!grocery_list_members.iter().any(|d| d.id == "member-echo-1"));
    assert!(!stores.iter().any(|d| d.id == "1002"));
    assert!(!categories.iter().any(|d| d.id == "2002"));
    assert!(!grocery_items.iter().any(|d| d.id == "3002"));
    assert!(!grocery_item_store_infos
        .iter()
        .any(|d| d.grocery_item_id == "3002" && d.store_id == "1002"));

    tx.rollback().await.unwrap();
}
