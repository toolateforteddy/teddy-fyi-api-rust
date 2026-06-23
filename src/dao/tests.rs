use crate::models::{Config, Drawing, SyncState};
use crate::dao::{ConfigDao, DrawingDao};
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test]
async fn test_config_dao_lifecycle(pool: PgPool) {
    let user_1 = Uuid::new_v4();
    let user_2 = Uuid::new_v4();
    let client_1 = Uuid::new_v4();
    let config_id = Uuid::new_v4();

    // 1. Insert a new config for user_1
    let config = Config {
        id: config_id,
        user_id: user_1,
        client_uuid: client_1,
        version: 1,
        is_deleted: false,
        last_modified: 1000,
        sync_state: SyncState::PendingInsert,
        key: "theme".to_string(),
        value: "dark".to_string(),
    };

    let inserted = ConfigDao::upsert(&pool, user_1, &config).await.unwrap();
    assert_eq!(inserted.id, config_id);
    assert_eq!(inserted.user_id, user_1);
    assert_eq!(inserted.version, 1);
    assert_eq!(inserted.value, "dark");

    // 2. Data Isolation Check: user_2 tries to read it and gets None
    let fetched_user_2 = ConfigDao::get_by_id(&pool, config_id, user_2).await.unwrap();
    assert!(fetched_user_2.is_none());

    // 3. Read it for user_1: succeeds
    let fetched_user_1 = ConfigDao::get_by_id(&pool, config_id, user_1).await.unwrap().unwrap();
    assert_eq!(fetched_user_1.value, "dark");

    // 4. Upsert with matching version: increments version
    let mut update_config = fetched_user_1.clone();
    update_config.value = "light".to_string();
    update_config.last_modified = 2000;
    
    let updated = ConfigDao::upsert(&pool, user_1, &update_config).await.unwrap();
    assert_eq!(updated.version, 2);
    assert_eq!(updated.value, "light");

    // 5. Conflict Resolution (LWW Wins): lower version, but newer timestamp
    let mut conflict_config_lww = updated.clone();
    conflict_config_lww.version = 1; // client has old version counter
    conflict_config_lww.value = "blue".to_string();
    conflict_config_lww.last_modified = 3000; // but client has newer timestamp

    let resolved_lww = ConfigDao::upsert(&pool, user_1, &conflict_config_lww).await.unwrap();
    assert_eq!(resolved_lww.version, 3); // overwrote and version bumped to server_version (2) + 1
    assert_eq!(resolved_lww.value, "blue");

    // 6. Conflict Resolution (Server Wins): lower version, older timestamp
    let mut conflict_config_server = resolved_lww.clone();
    conflict_config_server.version = 2; // client has old version
    conflict_config_server.value = "red".to_string();
    conflict_config_server.last_modified = 2500; // client timestamp is older than server's (3000)

    let resolved_server = ConfigDao::upsert(&pool, user_1, &conflict_config_server).await.unwrap();
    assert_eq!(resolved_server.version, 3); // rejected incoming, kept server's version (3)
    assert_eq!(resolved_server.value, "blue"); // kept server value

    // 7. Get Pending Sync (should be fetched because sync_state != SYNCED)
    let pending = ConfigDao::get_pending_sync(&pool, user_1, client_1).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, config_id);

    // 8. Soft Delete
    let deleted = ConfigDao::soft_delete(&pool, config_id, user_1, client_1, 4000).await.unwrap().unwrap();
    assert!(deleted.is_deleted);
    assert_eq!(deleted.version, 4);

    // Verify it is not listed in active configs
    let active = ConfigDao::list_for_user(&pool, user_1).await.unwrap();
    assert_eq!(active.len(), 0);
}

#[sqlx::test]
async fn test_drawing_dao_lifecycle(pool: PgPool) {
    let user_1 = Uuid::new_v4();
    let client_1 = Uuid::new_v4();
    let drawing_id = Uuid::new_v4();

    let drawing = Drawing {
        id: drawing_id,
        user_id: user_1,
        client_uuid: client_1,
        version: 1,
        is_deleted: false,
        last_modified: 1000,
        sync_state: SyncState::PendingInsert,
        created_at: 1000,
        data: serde_json::json!({ "strokes": [] }),
    };

    // 1. Insert drawing
    let inserted = DrawingDao::upsert(&pool, user_1, &drawing).await.unwrap();
    assert_eq!(inserted.id, drawing_id);
    assert_eq!(inserted.version, 1);

    // 2. Read drawing
    let fetched = DrawingDao::get_by_id(&pool, drawing_id, user_1).await.unwrap().unwrap();
    assert_eq!(fetched.data, serde_json::json!({ "strokes": [] }));

    // 3. Upsert update
    let mut updated_drawing = fetched;
    updated_drawing.data = serde_json::json!({ "strokes": [1, 2, 3] });
    updated_drawing.last_modified = 2000;

    let updated = DrawingDao::upsert(&pool, user_1, &updated_drawing).await.unwrap();
    assert_eq!(updated.version, 2);
    assert_eq!(updated.data, serde_json::json!({ "strokes": [1, 2, 3] }));

    // 4. Soft Delete
    let deleted = DrawingDao::soft_delete(&pool, drawing_id, user_1, client_1, 3000).await.unwrap().unwrap();
    assert!(deleted.is_deleted);
    assert_eq!(deleted.version, 3);
}
