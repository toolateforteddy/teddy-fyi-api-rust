//! The device a sync request speaks for comes from the token, not from the body.
//!
//! `SyncRequest.client_id` used to be an unverified field that decided three things: what
//! `client_uuid` a written row is stamped with, what `sender_client_id` the SSE fan-out
//! labels the change with, and — because both echo filters are `client_uuid != <caller>` —
//! which of an account's devices is deliberately *not* told about it. An authenticated caller
//! could therefore write as one of their other devices and have the update suppressed on
//! exactly the device it was aimed at. These tests pin the binding that closed that.

use axum::{extract::State, Extension};
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::tokens::Claims;
use crate::routes::sync::tests::helpers::{seed_device, setup_state};
use crate::routes::sync::{
    parse_or_hash_uuid, AppJson, ConfigSyncItem, SyncRequest, SyncScope,
};

fn claims_for(client_uuid: &str) -> Claims {
    Claims {
        sub: "user-1".to_string(),
        client_uuid: client_uuid.to_string(),
        exp: 10_000_000_000,
        product: None,
    }
}

/// A request with no changes in it, for `scope`, as `client_id` claims to be.
fn empty_request(client_id: &str, scope: SyncScope, since: Option<chrono::DateTime<Utc>>) -> SyncRequest {
    SyncRequest {
        last_synced_at: since,
        client_id: client_id.to_string(),
        device_uuid: None,
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
        configs: vec![],
        drawings: vec![],
        supports_paging: false,
    }
}

#[sqlx::test]
async fn body_client_id_is_overridden_by_the_token_and_cannot_poison_echo_filtering(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    let _device = seed_device(&pool, user_uuid, "Tablet").await;

    let attacker = "client-attacker";
    let victim = "client-victim";
    let config_id = Uuid::new_v4();

    // The write: authenticated as `attacker`, but the body names `victim`.
    let mut req = empty_request(victim, SyncScope::ScribbleKeep, None);
    req.configs = vec![ConfigSyncItem {
        id: config_id,
        device_uuid: None,
        key: "child_name".to_string(),
        value: "set-by-the-other-device".to_string(),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    }];

    let _ = crate::routes::sync::sync_handler(
        State(state.clone()),
        Extension(claims_for(attacker)),
        AppJson(req),
    )
    .await
    .expect("sync should succeed — the body field is corrected, not refused");

    // 1. The row is attributed to the device that actually authenticated.
    let stored = sqlx::query_scalar!("SELECT client_uuid FROM configs WHERE id = $1", config_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        stored,
        parse_or_hash_uuid(attacker),
        "the row must carry the token's client, not the body's"
    );
    assert_ne!(stored, parse_or_hash_uuid(victim));

    // 2. And so the victim device still hears about the change. This is the half that
    //    matters: attribution is only interesting because the echo filter reads it, and a row
    //    stamped `victim` would be filtered out of exactly this response as the victim's own
    //    echo — leaving a tablet silently stale.
    let poll = empty_request(
        victim,
        SyncScope::ScribbleKeep,
        Some(Utc::now() - chrono::Duration::minutes(5)),
    );
    let res = crate::routes::sync::sync_handler(
        State(state),
        Extension(claims_for(victim)),
        AppJson(poll),
    )
    .await
    .expect("victim sync should succeed")
    .0;

    let saw_it = res.configs.iter().any(|c| c.id == config_id)
        || res.remote_config_changes.iter().any(|c| c.id == config_id.to_string());
    assert!(
        saw_it,
        "the device the write was aimed away from must still receive it; got configs={:?} remote={:?}",
        res.configs.len(),
        res.remote_config_changes.len()
    );
}

/// The ordinary case — every client we ship sends the two identically — must be untouched.
#[sqlx::test]
async fn a_matching_body_client_id_behaves_exactly_as_before(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    let _device = seed_device(&pool, user_uuid, "Tablet").await;

    let config_id = Uuid::new_v4();
    let mut req = empty_request("client-1", SyncScope::ScribbleKeep, None);
    req.configs = vec![ConfigSyncItem {
        id: config_id,
        device_uuid: None,
        key: "child_name".to_string(),
        value: "ordinary".to_string(),
        sync_state: "PENDING_INSERT".to_string(),
        version: 1,
        is_deleted: false,
        last_modified: Utc::now().timestamp_millis(),
    }];

    let _ = crate::routes::sync::sync_handler(
        State(state),
        Extension(claims_for("client-1")),
        AppJson(req),
    )
    .await
    .expect("sync should succeed");

    let stored = sqlx::query_scalar!("SELECT client_uuid FROM configs WHERE id = $1", config_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, parse_or_hash_uuid("client-1"));
}
