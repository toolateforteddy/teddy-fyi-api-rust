use sqlx::PgPool;
use axum::extract::State;
use chrono::Utc;
use crate::routes::sync::tests::helpers::{setup_state, sync_handler};
use crate::routes::sync::{
    SyncRequest, SyncScope, DrawingData, DrawingChangeDelta, ConfigData, ConfigChangeDelta,
    OperationType, AppJson, parse_or_hash_uuid
};

#[sqlx::test]
async fn test_sync_handler_scribble_box(pool: PgPool) {
    let state = setup_state(pool.clone());
    let other_client = "client-2";
    let other_client_uuid = parse_or_hash_uuid(other_client);
    let user_uuid = parse_or_hash_uuid("user-1");

    // 1. Setup DB with a remote config change (should be downloaded)
    sqlx::query!(
        "INSERT INTO configs (id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, key, value) \
         VALUES ($1, $2, $3, $4, $5, $6, 'SYNCED'::sync_state, $7, $8)",
        uuid::Uuid::new_v4(),
        user_uuid,
        other_client_uuid,
        1_i32,
        false,
        Utc::now().timestamp_millis(),
        "config_key_1",
        "config_value_1"
    )
    .execute(&pool)
    .await
    .unwrap();

    // 2. Setup DB with a remote drawing change (should NOT be downloaded by ScribbleBox)
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

    // 3. Prepare client sync request: uploads drawing
    let drawing_id = uuid::Uuid::new_v4();
    let drawing_data = DrawingData {
        id: drawing_id,
        user_id: user_uuid.to_string(),
        client_uuid: parse_or_hash_uuid("client-1").to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
        sync_state: "SYNCED".to_string(),
        created_at: 1000,
        data: serde_json::json!({ "strokes": [1, 2] }),
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
        drawing_changes: vec![DrawingChangeDelta {
            id: drawing_id.to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&drawing_data).unwrap()),
        }],
        configs: vec![],
        drawings: vec![],
    };

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    // Verify drawing was uploaded
    assert!(res.success_ids.contains(&drawing_id.to_string()));

    // Verify config was downloaded, but drawing was not
    assert!(!res.remote_config_changes.is_empty());
    assert!(res.remote_drawing_changes.is_empty());

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
async fn test_sync_handler_scribble_keep(pool: PgPool) {
    let state = setup_state(pool.clone());
    let other_client = "client-2";
    let other_client_uuid = parse_or_hash_uuid(other_client);
    let user_uuid = parse_or_hash_uuid("user-1");

    // 1. Setup DB with remote config (downloadable) and drawing (NOT downloadable)
    sqlx::query!(
        "INSERT INTO configs (id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, key, value) \
         VALUES ($1, $2, $3, $4, $5, $6, 'SYNCED'::sync_state, $7, $8)",
        uuid::Uuid::new_v4(),
        user_uuid,
        other_client_uuid,
        1_i32,
        false,
        Utc::now().timestamp_millis(),
        "config_key_2",
        "config_value_2"
    )
    .execute(&pool)
    .await
    .unwrap();

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

    // 2. Prepare request: uploads config
    let config_id = uuid::Uuid::new_v4();
    let config_data = ConfigData {
        id: config_id,
        user_id: user_uuid.to_string(),
        client_uuid: parse_or_hash_uuid("client-1").to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
        sync_state: "SYNCED".to_string(),
        key: "theme".to_string(),
        value: "light".to_string(),
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
        config_changes: vec![ConfigChangeDelta {
            id: config_id.to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&config_data).unwrap()),
        }],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    // Verify config was uploaded
    assert!(res.success_ids.contains(&config_id.to_string()));

    // Verify configs downloaded, drawings not
    assert!(!res.remote_config_changes.is_empty());
    assert!(res.remote_drawing_changes.is_empty());

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
async fn test_sync_handler_scribble_keep_cloud(pool: PgPool) {
    let state = setup_state(pool.clone());
    let other_client = "client-2";
    let other_client_uuid = parse_or_hash_uuid(other_client);
    let user_uuid = parse_or_hash_uuid("user-1");

    // 1. Setup DB with remote config and drawing (BOTH downloadable)
    sqlx::query!(
        "INSERT INTO configs (id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, key, value) \
         VALUES ($1, $2, $3, $4, $5, $6, 'SYNCED'::sync_state, $7, $8)",
        uuid::Uuid::new_v4(),
        user_uuid,
        other_client_uuid,
        1_i32,
        false,
        Utc::now().timestamp_millis(),
        "config_key_3",
        "config_value_3"
    )
    .execute(&pool)
    .await
    .unwrap();

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

    // 2. Prepare request: uploads config (ScribbleKeepCloud only publishes configs)
    let config_id = uuid::Uuid::new_v4();
    let config_data = ConfigData {
        id: config_id,
        user_id: user_uuid.to_string(),
        client_uuid: parse_or_hash_uuid("client-1").to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
        sync_state: "SYNCED".to_string(),
        key: "editor_font".to_string(),
        value: "monospace".to_string(),
    };

    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        client_id: "client-1".to_string(),
        scope: Some(SyncScope::ScribbleKeepCloud),
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
        config_changes: vec![ConfigChangeDelta {
            id: config_id.to_string(),
            operation_type: OperationType::Insert,
            version: 1,
            data: Some(serde_json::to_value(&config_data).unwrap()),
        }],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
    };

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    // Verify upload success
    assert!(res.success_ids.contains(&config_id.to_string()));

    // Verify configs AND drawings downloaded
    assert!(!res.remote_config_changes.is_empty());
    assert!(!res.remote_drawing_changes.is_empty());
}
