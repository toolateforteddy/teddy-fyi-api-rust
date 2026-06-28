use sqlx::PgPool;
use axum::extract::State;
use chrono::Utc;
use crate::routes::sync::tests::helpers::{setup_state, sync_handler};
use crate::routes::sync::{
    SyncRequest, SyncScope, ConfigSyncItem, DrawingSyncItem, AppJson, parse_or_hash_uuid
};

#[sqlx::test]
async fn test_sync_handler_flat_configs(pool: PgPool) {
    let state = setup_state(pool.clone());
    let other_client = "client-2";
    let other_client_uuid = parse_or_hash_uuid(other_client);
    let user_uuid = parse_or_hash_uuid("user-1");

    // 1. Setup DB with remote config
    sqlx::query!(
        "INSERT INTO configs (id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, key, value) \
         VALUES ($1, $2, $3, $4, $5, $6, 'SYNCED'::sync_state, $7, $8)",
        uuid::Uuid::new_v4(),
        user_uuid,
        other_client_uuid,
        1_i32,
        false,
        Utc::now().timestamp_millis(),
        "theme",
        "dark"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 2. Prepare request with flat configs list
    let config_id = uuid::Uuid::new_v4();
    let config_item = ConfigSyncItem {
        id: config_id,
        key: "font_size".to_string(),
        value: "14".to_string(),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    };

    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        client_id: "client-1".to_string(),
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
        configs: vec![config_item],
        drawings: vec![],
    };

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    // Verify configs returned contains BOTH our uploaded config and the other client's config
    let returned_ids: Vec<uuid::Uuid> = res.configs.iter().map(|c| c.id).collect();
    assert!(returned_ids.contains(&config_id));
    assert_eq!(res.configs.len(), 2);

    // Verify config is in DB
    let count = sqlx::query!("SELECT count(*) FROM configs WHERE id = $1", config_id)
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn test_sync_handler_flat_drawings(pool: PgPool) {
    let state = setup_state(pool.clone());
    let other_client = "client-2";
    let other_client_uuid = parse_or_hash_uuid(other_client);
    let user_uuid = parse_or_hash_uuid("user-1");

    // 1. Setup DB with remote drawing
    sqlx::query!(
        "INSERT INTO drawings (id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data) \
         VALUES ($1, $2, $3, $4, $5, $6, 'SYNCED'::sync_state, $7, $8)",
        uuid::Uuid::new_v4(),
        user_uuid,
        other_client_uuid,
        1_i32,
        false,
        Utc::now().timestamp_millis(),
        1000_i64,
        serde_json::json!({ "strokes": [] })
    )
    .execute(&pool)
    .await
    .unwrap();

    // 2. Prepare request with flat drawings list (ScribbleBox uploads drawing)
    let drawing_id = uuid::Uuid::new_v4();
    let drawing_item = DrawingSyncItem {
        id: drawing_id,
        user_id: Some(user_uuid.to_string()),
        created_at: 1000,
        data: serde_json::json!({ "strokes": [1] }),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    };

    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        client_id: "client-1".to_string(),
        scope: Some(SyncScope::ScribbleBox),
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
        drawings: vec![drawing_item],
    };

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    // Verify Under ScribbleBox, only our own uploaded drawing is returned, no remote drawings
    let returned_ids: Vec<uuid::Uuid> = res.drawings.iter().map(|d| d.id).collect();
    assert!(returned_ids.contains(&drawing_id));
    assert_eq!(res.drawings.len(), 1);

    // Verify drawing is in DB
    let count = sqlx::query!("SELECT count(*) FROM drawings WHERE id = $1", drawing_id)
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn test_sync_handler_flat_drawings_non_uuid_user_id(pool: PgPool) {
    let state = setup_state(pool.clone());
    let drawing_id = uuid::Uuid::new_v4();

    // Prepare request with a non-UUID user_id string ("toddler_1")
    let drawing_item = DrawingSyncItem {
        id: drawing_id,
        user_id: Some("toddler_1".to_string()),
        created_at: 1000,
        data: serde_json::json!({ "strokes": [1] }),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    };

    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        client_id: "client-1".to_string(),
        scope: Some(SyncScope::ScribbleBox),
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
        drawings: vec![drawing_item],
    };

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed and ignore/overwrite invalid user_id")
        .0;

    let returned_ids: Vec<uuid::Uuid> = res.drawings.iter().map(|d| d.id).collect();
    assert!(returned_ids.contains(&drawing_id));

    // Verify drawing is in DB owned by user_uuid (which is hashed from "user-1" because sync_handler uses Claims with sub "user-1")
    let user_uuid = parse_or_hash_uuid("user-1");
    let row = sqlx::query!("SELECT user_id FROM drawings WHERE id = $1", drawing_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.user_id, user_uuid);
}
