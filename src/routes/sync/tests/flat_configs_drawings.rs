use sqlx::PgPool;
use axum::extract::State;
use chrono::Utc;
use crate::routes::sync::tests::helpers::{request, seed_device, setup_state, sync_handler};
use crate::routes::sync::{
    SyncRequest, SyncScope, ConfigSyncItem, DrawingSyncItem, AppJson, parse_or_hash_uuid
};

#[sqlx::test]
async fn test_sync_handler_flat_configs(pool: PgPool) {
    let state = setup_state(pool.clone());
    let other_client = "client-2";
    let other_client_uuid = parse_or_hash_uuid(other_client);
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;

    // 1. Setup DB with remote config
    sqlx::query!(
        "INSERT INTO configs (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state, key, value) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'SYNCED'::sync_state, $8, $9)",
        uuid::Uuid::new_v4(),
        user_uuid,
        device_uuid,
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
        device_uuid: None,
        key: "font_size".to_string(),
        value: "14".to_string(),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    };

    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        scope: Some(SyncScope::ScribbleKeep),
        configs: vec![config_item],
        ..request("client-1")
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
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;

    // 1. Setup DB with remote drawing
    sqlx::query!(
        "INSERT INTO drawings (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'SYNCED'::sync_state, $8, $9)",
        uuid::Uuid::new_v4(),
        user_uuid,
        device_uuid,
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
        device_uuid: None,
        created_at: 1000,
        data: serde_json::json!({ "strokes": [1] }),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    };

    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        scope: Some(SyncScope::ScribbleBox),
        drawings: vec![drawing_item],
        ..request("client-1")
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
        device_uuid: None,
        created_at: 1000,
        data: serde_json::json!({ "strokes": [1] }),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    };

    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        scope: Some(SyncScope::ScribbleBox),
        drawings: vec![drawing_item],
        ..request("client-1")
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

#[sqlx::test]
async fn test_sync_handler_flat_drawings_scribble_keep(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    let _device_uuid = seed_device(&pool, user_uuid, "Tablet").await;

    // Drawings now arrive under ScribbleKeep, alongside the configs that scope already carried.
    let drawing_id = uuid::Uuid::new_v4();
    let drawing_item = DrawingSyncItem {
        id: drawing_id,
        user_id: Some("toddler_1".to_string()),
        device_uuid: None,
        created_at: 1000,
        data: serde_json::json!({ "strokes": [1] }),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    };

    let config_id = uuid::Uuid::new_v4();
    let config_item = ConfigSyncItem {
        id: config_id,
        device_uuid: None,
        key: "theme".to_string(),
        value: "light".to_string(),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    };

    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        scope: Some(SyncScope::ScribbleKeep),
        configs: vec![config_item],
        drawings: vec![drawing_item],
        ..request("client-1")
    };

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    // The uploaded drawing is echoed back, and no remote drawings leak into the Keep scope.
    let returned_ids: Vec<uuid::Uuid> = res.drawings.iter().map(|d| d.id).collect();
    assert!(returned_ids.contains(&drawing_id));
    assert_eq!(res.drawings.len(), 1);

    // Both the drawing and the config landed in the DB under the parent's user id.
    let drawing_owner = sqlx::query!("SELECT user_id FROM drawings WHERE id = $1", drawing_id)
        .fetch_one(&pool)
        .await
        .unwrap()
        .user_id;
    assert_eq!(drawing_owner, user_uuid);

    let config_count = sqlx::query!("SELECT count(*) FROM configs WHERE id = $1", config_id)
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
    assert_eq!(config_count, 1);
}

/// Two configs in one payload claiming the same key.
///
/// The write paths prefetch what is on the server once per batch rather than once per item,
/// which means the second of these two is resolved against a picture taken before the first
/// one was written. The unique key is `(user_id, device_uuid, key)`, so getting that wrong
/// is not a stale read — it is a constraint violation that fails the whole request.
///
/// The behaviour being pinned is the one the per-item resolution had: the second item lands
/// on the row the first created and reconciles it onto the id it submitted.
#[sqlx::test]
async fn two_configs_in_one_payload_contending_for_a_key(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;

    let first_id = uuid::Uuid::new_v4();
    let second_id = uuid::Uuid::new_v4();
    let config = |id: uuid::Uuid, value: &str| ConfigSyncItem {
        id,
        device_uuid: None,
        key: "theme".to_string(),
        value: value.to_string(),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    };

    let mut req = blank_request();
    req.configs = vec![config(first_id, "dark"), config(second_id, "light")];

    let _ = sync_handler(State(state), AppJson(req))
        .await
        .expect("a payload with two configs on one key must not fail the request");

    // One row on the key, not two and not a duplicate-key error: the second item updated
    // the row the first inserted, taking its own id and advancing the version.
    let rows = sqlx::query!(
        "SELECT id, value, version FROM configs WHERE user_id = $1 AND key = 'theme'",
        user_uuid
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "expected one row on the key, got {:?}", rows.len());
    assert_eq!(rows[0].id, second_id);
    assert_eq!(rows[0].value, "light");
    assert_eq!(rows[0].version, 2);
}

/// Two configs in one payload claiming the same id under different keys.
///
/// The mirror of the case above, and the one that trips `configs_pkey` rather than the
/// unique key: `id` is the primary key, so the second item has to see that the first has
/// already taken it. Per the per-item resolution, it lands on that row and renames its key.
#[sqlx::test]
async fn two_configs_in_one_payload_contending_for_an_id(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;

    let shared_id = uuid::Uuid::new_v4();
    let config = |key: &str, value: &str| ConfigSyncItem {
        id: shared_id,
        device_uuid: None,
        key: key.to_string(),
        value: value.to_string(),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    };

    let mut req = blank_request();
    req.configs = vec![config("child_name", "ada"), config("lockout_minutes", "20")];

    let _ = sync_handler(State(state), AppJson(req))
        .await
        .expect("a payload with two configs on one id must not fail the request");

    let rows = sqlx::query!(
        "SELECT id, key, value, version FROM configs WHERE user_id = $1",
        user_uuid
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "expected one row on the id, got {:?}", rows.len());
    assert_eq!(rows[0].id, shared_id);
    assert_eq!(rows[0].key, "lockout_minutes");
    assert_eq!(rows[0].value, "20");
    assert_eq!(rows[0].version, 2);
}

/// The same drawing twice in one payload.
///
/// Same shape of trap on the drawing side: the second upload has to be versioned against
/// what the first one wrote, not against the prefetch taken before either ran.
#[sqlx::test]
async fn the_same_drawing_twice_in_one_payload_versions_off_the_first(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "Tablet").await;

    let drawing_id = uuid::Uuid::new_v4();
    let drawing = |stroke: i32| DrawingSyncItem {
        id: drawing_id,
        user_id: Some(user_uuid.to_string()),
        device_uuid: None,
        created_at: 1000,
        data: serde_json::json!({ "strokes": [stroke] }),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    };

    let mut req = blank_request();
    req.scope = Some(SyncScope::ScribbleBox);
    req.drawings = vec![drawing(1), drawing(2)];

    let _ = sync_handler(State(state), AppJson(req))
        .await
        .expect("a payload naming one drawing twice must not fail the request");

    let row = sqlx::query!(
        "SELECT version, data FROM drawings WHERE id = $1",
        drawing_id
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.version, 2);
    assert_eq!(row.data, serde_json::json!({ "strokes": [2] }));
}

/// An otherwise empty ScribbleKeep request from `client-1`, for the tests above to fill in.
fn blank_request() -> SyncRequest {
    SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        scope: Some(SyncScope::ScribbleKeep),
        ..request("client-1")
    }
}
