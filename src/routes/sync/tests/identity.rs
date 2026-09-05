//! Characterisation tests for the two user identities described in
//! `context/2026-09-05_user_identity_derivation.md`.
//!
//! Nothing here asserts that the current design is *good*. These tests exist so that a
//! future change to `parse_or_hash_uuid` — or to which identity a table is keyed by —
//! fails loudly here instead of silently orphaning every config, drawing and device row
//! already in the database. If one of these fails, the change under way is a data
//! migration, not a refactor.

use sqlx::PgPool;
use axum::extract::State;
use chrono::Utc;
use uuid::Uuid;

use crate::routes::sync::tests::helpers::{setup_state, sync_handler, seed_device};
use crate::routes::sync::{
    AppJson, ConfigSyncItem, OperationType, SyncRequest, SyncScope, TodoListChangeDelta,
    TodoListData, parse_or_hash_uuid,
};

/// The literals below are the derivation itself, not a value copied out of a test run.
/// `uuid5(NAMESPACE_DNS, subject)` is unkeyed and deterministic, so if a future edit
/// changes the namespace, the hash, or the input encoding, these constants stop matching
/// and every existing row keyed by the old value is stranded.
const USER_1_DERIVED: &str = "d35a2a2a-d1d1-55ed-90a7-348c3da59deb";
/// Shaped like a real Google `sub`: 21 decimal digits, which is not a UUID, so it hashes.
const GOOGLE_SUB: &str = "104928374650192837465";
const GOOGLE_SUB_DERIVED: &str = "58a6ea82-3e42-5ec5-867a-f4f433656ff6";

#[test]
fn derivation_is_stable_for_a_given_subject() {
    assert_eq!(
        parse_or_hash_uuid("user-1"),
        Uuid::parse_str(USER_1_DERIVED).unwrap(),
        "the config/drawing/device key for a subject must never move"
    );
    assert_eq!(
        parse_or_hash_uuid(GOOGLE_SUB),
        Uuid::parse_str(GOOGLE_SUB_DERIVED).unwrap()
    );
    // Deterministic across calls, which is the whole reason it can be used as a key
    // without ever being stored.
    assert_eq!(parse_or_hash_uuid(GOOGLE_SUB), parse_or_hash_uuid(GOOGLE_SUB));
}

#[test]
fn distinct_subjects_derive_distinct_uuids() {
    assert_ne!(parse_or_hash_uuid("user-1"), parse_or_hash_uuid("user-2"));
    // Byte-for-byte input: no trimming, no case folding, no normalisation. Two subjects
    // that differ only in whitespace or case are two different accounts here.
    assert_ne!(parse_or_hash_uuid("user-1"), parse_or_hash_uuid("user-1 "));
    assert_ne!(parse_or_hash_uuid("user-1"), parse_or_hash_uuid("User-1"));
}

#[test]
fn a_uuid_shaped_subject_passes_through_unchanged() {
    let already = Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
    assert_eq!(parse_or_hash_uuid(&already.to_string()), already);

    // `Uuid::parse_str` accepts more spellings than the canonical one, and all of them
    // land on the same identifier — the hashing branch is never reached for any of them.
    assert_eq!(parse_or_hash_uuid("11111111222233334444555555555555"), already);
    assert_eq!(parse_or_hash_uuid("11111111-2222-3333-4444-555555555555".to_uppercase().as_str()), already);
}

/// The two branches share one output space, so a subject that *is* the textual form of
/// another subject's derived UUID resolves to that same identifier. Google `sub` values
/// are decimal digit strings and can never take this shape, so this is not reachable
/// through a real sign-in — but it is reachable through any code path that lets a caller
/// choose its own subject, which is what the `mock.` dev bypass in `auth::handlers` does.
#[test]
fn the_two_branches_share_one_identifier_space() {
    let derived = parse_or_hash_uuid("user-1");
    assert_eq!(parse_or_hash_uuid(&derived.to_string()), derived);
}

/// The split itself: one authenticated subject, two identities, in one database.
///
/// Todo (and grocery) rows are keyed by the raw subject; config, drawing and device rows
/// are keyed by its derived UUID. Any future code that assumes a single identity will read
/// or write the wrong rows, so this pins both halves at once.
#[sqlx::test]
async fn todo_rows_key_off_the_raw_subject_and_configs_off_the_derived_uuid(pool: PgPool) {
    let state = setup_state(pool.clone());
    // `sync_handler` in `helpers` authenticates as sub `user-1` / client `client-1`.
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;

    let list_data = TodoListData {
        id: "identity-list".to_string(),
        name: "List".to_string(),
        color_hex: "#FF0000".to_string(),
        user_id: Some("user-1".to_string()),
        created_at: 0,
        sync_state: "SYNCED".to_string(),
        version: 1,
        is_deleted: false,
    };

    let todo_req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
        scope: Some(SyncScope::Todo),
        todo_list_changes: vec![TodoListChangeDelta {
            id: "identity-list".to_string(),
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
        supports_paging: false,
    };

    let _ = sync_handler(State(state.clone()), AppJson(todo_req))
        .await
        .expect("todo sync should succeed");

    let config_id = Uuid::new_v4();
    let config_req = SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: Some(device_uuid),
        device_name: None,
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
        configs: vec![ConfigSyncItem {
            id: config_id,
            device_uuid: Some(device_uuid),
            key: "theme".to_string(),
            value: "dark".to_string(),
            sync_state: "PENDING_INSERT".to_string(),
            version: 1,
            is_deleted: false,
            last_modified: Utc::now().timestamp_millis(),
        }],
        drawings: vec![],
        supports_paging: false,
    };

    let _ = sync_handler(State(state), AppJson(config_req))
        .await
        .expect("config sync should succeed");

    // Raw subject, stored as text.
    let todo_owner = sqlx::query_scalar!(
        r#"SELECT "userId" FROM todo_lists WHERE id = $1"#,
        "identity-list"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(todo_owner.as_deref(), Some("user-1"));

    // Derived UUID, stored as uuid — a different column type holding a different value
    // for the same signed-in person.
    let config_owner = sqlx::query_scalar!("SELECT user_id FROM configs WHERE id = $1", config_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(config_owner, user_uuid);
    assert_ne!(config_owner.to_string(), "user-1");

    // And the device the config hangs off is keyed the same derived way, which is why the
    // stale-user reaper has to hash in application code to join `devices` back to `users`.
    let device_owner = sqlx::query_scalar!("SELECT user_id FROM devices WHERE id = $1", device_uuid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(device_owner, user_uuid);
}
