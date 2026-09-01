use axum::extract::{Path, State};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::tokens::Claims;
use crate::routes::devices::handlers::{
    list_devices_handler, rename_device_handler, RenameDeviceRequest,
};
use crate::routes::sync::tests::helpers::{seed_device, setup_state, sync_handler};
use crate::routes::sync::{
    AppError, AppJson, ConfigSyncItem, DrawingSyncItem, SyncRequest, SyncScope,
    parse_or_hash_uuid,
};

fn claims() -> Claims {
    Claims {
        sub: "user-1".to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10000000000,
    }
}

/// A sync request carrying nothing but the device identity and the given payload.
fn request(
    client_id: &str,
    device_uuid: Option<Uuid>,
    scope: SyncScope,
    configs: Vec<ConfigSyncItem>,
    drawings: Vec<DrawingSyncItem>,
) -> SyncRequest {
    SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        client_id: client_id.to_string(),
        device_uuid,
        device_name: None,
        scope: Some(scope),
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
        configs,
        drawings,
    }
}

fn config_item(key: &str, value: &str) -> ConfigSyncItem {
    ConfigSyncItem {
        id: Uuid::new_v4(),
        device_uuid: None,
        key: key.to_string(),
        value: value.to_string(),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    }
}

#[sqlx::test]
async fn test_two_devices_hold_the_same_config_key(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_a = seed_device(&pool, user_uuid, "BouncyMeadowAdventure").await;
    let device_b = seed_device(&pool, user_uuid, "SleepyRiverJourney").await;

    // Tablet A sets theme=dark, tablet B sets theme=light. Under the old
    // UNIQUE (user_id, key) these clobbered each other.
    let res_a = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-a",
            Some(device_a),
            SyncScope::ScribbleKeep,
            vec![config_item("theme", "dark")],
            vec![],
        )),
    )
    .await
    .expect("device A sync should succeed")
    .0;

    let res_b = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-b",
            Some(device_b),
            SyncScope::ScribbleKeep,
            vec![config_item("theme", "light")],
            vec![],
        )),
    )
    .await
    .expect("device B sync should succeed")
    .0;

    // Both rows survive, one per device.
    let rows = sqlx::query!(
        "SELECT device_uuid, value FROM configs WHERE user_id = $1 AND key = 'theme' ORDER BY value",
        user_uuid
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].value, "dark");
    assert_eq!(rows[0].device_uuid, device_a);
    assert_eq!(rows[1].value, "light");
    assert_eq!(rows[1].device_uuid, device_b);

    // And neither tablet sees the other's row under the Keep scope.
    assert!(res_a.configs.iter().all(|c| c.device_uuid == Some(device_a)));
    assert!(res_b.configs.iter().all(|c| c.device_uuid == Some(device_b)));
    assert!(res_b.configs.iter().all(|c| c.value != "dark"));
}

#[sqlx::test]
async fn test_keep_cloud_scope_reads_across_devices(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_a = seed_device(&pool, user_uuid, "BouncyMeadowAdventure").await;
    let device_b = seed_device(&pool, user_uuid, "SleepyRiverJourney").await;

    for (device_uuid, value) in [(device_a, "dark"), (device_b, "light")] {
        sqlx::query!(
            "INSERT INTO configs (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state, key, value) \
             VALUES ($1, $2, $3, $4, 1, FALSE, $5, 'SYNCED'::sync_state, 'theme', $6)",
            Uuid::new_v4(),
            user_uuid,
            device_uuid,
            parse_or_hash_uuid("client-other"),
            Utc::now().timestamp_millis(),
            value
        )
        .execute(&pool)
        .await
        .unwrap();

        sqlx::query!(
            "INSERT INTO drawings (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data) \
             VALUES ($1, $2, $3, $4, 1, FALSE, $5, 'SYNCED'::sync_state, 1000, $6)",
            Uuid::new_v4(),
            user_uuid,
            device_uuid,
            parse_or_hash_uuid("client-other"),
            Utc::now().timestamp_millis(),
            serde_json::json!({ "strokes": [] })
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    // The dashboard syncs as its own device but reads the whole account.
    let cloud_device = seed_device(&pool, user_uuid, "ParentDashboard").await;
    let res = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-cloud",
            Some(cloud_device),
            SyncScope::ScribbleKeepCloud,
            vec![],
            vec![],
        )),
    )
    .await
    .expect("cloud sync should succeed")
    .0;

    let config_devices: Vec<Option<Uuid>> = res.configs.iter().map(|c| c.device_uuid).collect();
    assert!(config_devices.contains(&Some(device_a)));
    assert!(config_devices.contains(&Some(device_b)));

    let drawing_devices: Vec<Option<Uuid>> = res.drawings.iter().map(|d| d.device_uuid).collect();
    assert!(drawing_devices.contains(&Some(device_a)));
    assert!(drawing_devices.contains(&Some(device_b)));

    // The deltas carry the device too, so the dashboard can attribute each row.
    assert!(res
        .remote_config_changes
        .iter()
        .any(|c| c.device_uuid == Some(device_a)));
    assert!(res
        .remote_drawing_changes
        .iter()
        .any(|d| d.device_uuid == Some(device_b)));
}

#[sqlx::test]
async fn test_write_to_another_users_device_is_rejected(pool: PgPool) {
    let other_user = Uuid::new_v4();
    let foreign_device = seed_device(&pool, other_user, "SomeoneElsesTablet").await;

    let err = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-a",
            Some(foreign_device),
            SyncScope::ScribbleKeep,
            vec![config_item("theme", "dark")],
            vec![],
        )),
    )
    .await
    .expect_err("writing into another account's device must be rejected");

    match err {
        AppError::Forbidden(msg) => assert!(msg.contains(&foreign_device.to_string())),
        other => panic!("expected Forbidden, got {:?}", other),
    }

    // Nothing leaked into the other account.
    let count = sqlx::query!(
        "SELECT count(*) FROM configs WHERE device_uuid = $1",
        foreign_device
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .count
    .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn test_write_without_device_uuid_uses_backfilled_device(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let backfilled = seed_device(&pool, user_uuid, "Tablet").await;
    // A second, newer device must not steal the fallback.
    let _newer = seed_device(&pool, user_uuid, "NewerTablet").await;

    let res = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-old",
            None,
            SyncScope::ScribbleKeep,
            vec![config_item("theme", "dark")],
            vec![],
        )),
    )
    .await
    .expect("an un-upgraded client should still sync")
    .0;

    assert!(res
        .configs
        .iter()
        .any(|c| c.key == "theme" && c.device_uuid == Some(backfilled)));

    let row = sqlx::query!(
        "SELECT device_uuid FROM configs WHERE user_id = $1 AND key = 'theme'",
        user_uuid
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.device_uuid, backfilled);
}

#[sqlx::test]
async fn test_sync_registers_device_and_touches_last_seen(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = Uuid::new_v4();

    let mut req = request(
        "client-a",
        Some(device_uuid),
        SyncScope::ScribbleKeep,
        vec![config_item("theme", "dark")],
        vec![],
    );
    req.device_name = Some("BouncyMeadowAdventure".to_string());

    let _ = sync_handler(State(setup_state(pool.clone())), AppJson(req))
        .await
        .expect("first sync should register the device");

    let row = sqlx::query!(
        "SELECT name, last_seen_at FROM devices WHERE id = $1 AND user_id = $2",
        device_uuid,
        user_uuid
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.name, "BouncyMeadowAdventure");
    assert!(row.last_seen_at.is_some(), "sync should touch last_seen_at");
}

#[sqlx::test]
async fn test_list_and_rename_devices(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "BouncyMeadowAdventure").await;
    let foreign_device = seed_device(&pool, Uuid::new_v4(), "SomeoneElsesTablet").await;

    let listed = list_devices_handler(State(setup_state(pool.clone())), axum::Extension(claims()))
        .await
        .expect("listing should succeed")
        .0;
    assert_eq!(listed.devices.len(), 1);
    assert_eq!(listed.devices[0].id, device_uuid);
    assert_eq!(listed.devices[0].name, "BouncyMeadowAdventure");
    assert!(listed.devices[0].last_seen_at.is_none());

    let renamed = rename_device_handler(
        State(setup_state(pool.clone())),
        axum::Extension(claims()),
        Path(device_uuid),
        axum::Json(RenameDeviceRequest {
            name: "KitchenTablet".to_string(),
        }),
    )
    .await
    .expect("rename should succeed")
    .0;
    assert_eq!(renamed.name, "KitchenTablet");

    // Renaming a device on another account is not found for this caller.
    let err = rename_device_handler(
        State(setup_state(pool.clone())),
        axum::Extension(claims()),
        Path(foreign_device),
        axum::Json(RenameDeviceRequest {
            name: "Hijacked".to_string(),
        }),
    )
    .await
    .expect_err("renaming another account's device must fail");
    assert!(matches!(err, AppError::NotFound(_)));
}

/// Pins the wire contract the Android clients are built against: snake_case on the way
/// out, camelCase accepted on the way in.
#[test]
fn test_device_uuid_wire_format() {
    let device_uuid = Uuid::parse_str("dddddddd-0000-0000-0000-00000000000d").unwrap();
    let item = ConfigSyncItem {
        device_uuid: Some(device_uuid),
        ..config_item("theme", "dark")
    };

    let json = serde_json::to_value(&item).unwrap();
    assert_eq!(
        json.get("device_uuid").and_then(|v| v.as_str()),
        Some("dddddddd-0000-0000-0000-00000000000d")
    );

    // camelCase input is accepted, and a payload with no device at all still parses.
    let camel: ConfigSyncItem = serde_json::from_value(serde_json::json!({
        "id": Uuid::new_v4(),
        "deviceUuid": device_uuid,
        "key": "theme",
        "value": "dark",
        "version": 1,
        "isDeleted": false,
        "lastModified": 1,
    }))
    .unwrap();
    assert_eq!(camel.device_uuid, Some(device_uuid));

    let absent: ConfigSyncItem = serde_json::from_value(serde_json::json!({
        "id": Uuid::new_v4(),
        "key": "theme",
        "value": "dark",
        "version": 1,
        "isDeleted": false,
        "lastModified": 1,
    }))
    .unwrap();
    assert_eq!(absent.device_uuid, None);
}

/// A tablet that pairs under a new device id keeps sending the config row ids it already
/// had. `configs.id` is a global primary key while the duplicate-id lookup was scoped to
/// the request's device, so the write used to insert a taken id and trip `configs_pkey`
/// with a 500 instead of giving the new device its own row.
#[sqlx::test]
async fn test_config_id_already_held_by_another_device(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_a = seed_device(&pool, user_uuid, "BouncyMeadowAdventure").await;
    let device_b = seed_device(&pool, user_uuid, "SleepyRiverJourney").await;

    let shared_id = Uuid::new_v4();

    let mut item_a = config_item("bug_catcher_num_colors", "4");
    item_a.id = shared_id;
    let _ = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-a",
            Some(device_a),
            SyncScope::ScribbleKeep,
            vec![item_a],
            vec![],
        )),
    )
    .await
    .expect("device A sync should succeed");

    let mut item_b = config_item("bug_catcher_num_colors", "7");
    item_b.id = shared_id;
    let res_b = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-b",
            Some(device_b),
            SyncScope::ScribbleKeep,
            vec![item_b],
            vec![],
        )),
    )
    .await
    .expect("device B sync should succeed rather than trip configs_pkey")
    .0;

    // Device A keeps its row and its id; device B gets its own row under a free id.
    let rows = sqlx::query!(
        "SELECT id, device_uuid, value FROM configs \
         WHERE user_id = $1 AND key = 'bug_catcher_num_colors' ORDER BY value",
        user_uuid
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].value, "4");
    assert_eq!(rows[0].device_uuid, device_a);
    assert_eq!(rows[0].id, shared_id);
    assert_eq!(rows[1].value, "7");
    assert_eq!(rows[1].device_uuid, device_b);
    assert_ne!(rows[1].id, shared_id);

    // The response tells device B the id its row actually landed on.
    let echoed = res_b
        .configs
        .iter()
        .find(|c| c.key == "bug_catcher_num_colors")
        .expect("device B should see its own config back");
    assert_eq!(echoed.device_uuid, Some(device_b));
    assert_eq!(echoed.value, "7");
    assert_ne!(echoed.id, shared_id);
}

/// The cloud dashboard is not a tablet, so a config write from it that names no device has
/// no correct home — the server must say so rather than filing it against whichever device
/// happens to be oldest on the account.
#[sqlx::test]
async fn test_cloud_config_write_without_device_is_rejected(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_a = seed_device(&pool, user_uuid, "BouncyMeadowAdventure").await;

    let err = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-cloud",
            None,
            SyncScope::ScribbleKeepCloud,
            vec![config_item("theme", "dark")],
            vec![],
        )),
    )
    .await
    .expect_err("a cloud config write without a device_uuid should be rejected");

    match err {
        AppError::BadRequest(msg) => assert!(msg.contains("device_uuid"), "got: {msg}"),
        other => panic!("expected BadRequest, got {other:?}"),
    }

    // Nothing was written against the account's existing device.
    let count = sqlx::query!(
        "SELECT COUNT(*) as count FROM configs WHERE user_id = $1 AND device_uuid = $2",
        user_uuid,
        device_a
    )
    .fetch_one(&pool)
    .await
    .unwrap()
    .count
    .unwrap();
    assert_eq!(count, 0);
}

/// Reading across devices does not license writing across them: a cloud write naming
/// device B must not land on device A's row even when it carries A's row id.
#[sqlx::test]
async fn test_cloud_write_cannot_reassign_another_devices_row(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_a = seed_device(&pool, user_uuid, "BouncyMeadowAdventure").await;
    let device_b = seed_device(&pool, user_uuid, "SleepyRiverJourney").await;

    let shared_id = Uuid::new_v4();
    let mut item_a = config_item("theme", "dark");
    item_a.id = shared_id;
    item_a.device_uuid = Some(device_a);
    let _ = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-cloud",
            None,
            SyncScope::ScribbleKeepCloud,
            vec![item_a],
            vec![],
        )),
    )
    .await
    .expect("seeding device A's row via cloud should succeed");

    // Same row id, but claimed for device B.
    let mut item_b = config_item("theme", "light");
    item_b.id = shared_id;
    item_b.device_uuid = Some(device_b);
    let _ = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-cloud",
            None,
            SyncScope::ScribbleKeepCloud,
            vec![item_b],
            vec![],
        )),
    )
    .await
    .expect("cloud write for device B should succeed");

    // Device A keeps its row untouched; device B gets its own under a free id.
    let rows = sqlx::query!(
        "SELECT id, device_uuid, value FROM configs \
         WHERE user_id = $1 AND key = 'theme' ORDER BY value",
        user_uuid
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].value, "dark");
    assert_eq!(rows[0].device_uuid, device_a);
    assert_eq!(rows[0].id, shared_id);
    assert_eq!(rows[1].value, "light");
    assert_eq!(rows[1].device_uuid, device_b);
    assert_ne!(rows[1].id, shared_id);
}

/// The cloud app's whole purpose: one request writing to several devices it manages, each
/// row landing on the device it names rather than on whatever host the app runs from.
#[sqlx::test]
async fn test_cloud_writes_each_row_to_the_device_it_names(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_a = seed_device(&pool, user_uuid, "BouncyMeadowAdventure").await;
    let device_b = seed_device(&pool, user_uuid, "SleepyRiverJourney").await;

    let mut for_a = config_item("theme", "dark");
    for_a.device_uuid = Some(device_a);
    let mut for_b = config_item("theme", "light");
    for_b.device_uuid = Some(device_b);

    let _ = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-cloud",
            None,
            SyncScope::ScribbleKeepCloud,
            vec![for_a, for_b],
            vec![],
        )),
    )
    .await
    .expect("cloud should write to both managed devices in one request");

    let rows = sqlx::query!(
        "SELECT device_uuid, value FROM configs \
         WHERE user_id = $1 AND key = 'theme' ORDER BY value",
        user_uuid
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].value, "dark");
    assert_eq!(rows[0].device_uuid, device_a);
    assert_eq!(rows[1].value, "light");
    assert_eq!(rows[1].device_uuid, device_b);
}

/// The cloud app can only manage tablets that already exist. A stale or mistyped device id
/// is an error, not an invitation to register a device that never synced.
#[sqlx::test]
async fn test_cloud_write_to_unregistered_device_is_rejected(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    seed_device(&pool, user_uuid, "BouncyMeadowAdventure").await;
    let stranger = Uuid::new_v4();

    let mut item = config_item("theme", "dark");
    item.device_uuid = Some(stranger);

    let err = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-cloud",
            None,
            SyncScope::ScribbleKeepCloud,
            vec![item],
            vec![],
        )),
    )
    .await
    .expect_err("an unregistered device id should be rejected");

    match err {
        AppError::NotFound(msg) => assert!(msg.contains(&stranger.to_string()), "got: {msg}"),
        other => panic!("expected NotFound, got {other:?}"),
    }

    // No phantom device was conjured, and nothing was written.
    let devices = sqlx::query!("SELECT COUNT(*) as count FROM devices WHERE user_id = $1", user_uuid)
        .fetch_one(&pool).await.unwrap().count.unwrap();
    assert_eq!(devices, 1);
    let configs = sqlx::query!("SELECT COUNT(*) as count FROM configs WHERE user_id = $1", user_uuid)
        .fetch_one(&pool).await.unwrap().count.unwrap();
    assert_eq!(configs, 0);
}

/// A cloud sync is not a tablet checking in: it must not register the host it runs on, nor
/// mark a managed device as recently seen.
#[sqlx::test]
async fn test_cloud_sync_registers_and_touches_no_device(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_a = seed_device(&pool, user_uuid, "BouncyMeadowAdventure").await;
    let cloud_host = Uuid::new_v4();

    let mut item = config_item("theme", "dark");
    item.device_uuid = Some(device_a);

    // The request names the cloud app's own host; it is not a tablet on this account.
    let _ = sync_handler(
        State(setup_state(pool.clone())),
        AppJson(request(
            "client-cloud",
            Some(cloud_host),
            SyncScope::ScribbleKeepCloud,
            vec![item],
            vec![],
        )),
    )
    .await
    .expect("cloud sync should succeed");

    // The host was not registered as a device.
    let rows = sqlx::query!("SELECT id, last_seen_at FROM devices WHERE user_id = $1", user_uuid)
        .fetch_all(&pool).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, device_a);
    // And editing device A remotely did not mark it as having synced.
    assert!(rows[0].last_seen_at.is_none());
}
