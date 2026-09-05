//! The product boundary, through the real handlers.
//!
//! `src/routes/sync/scope_auth.rs` unit-tests the rule; these prove it is actually wired
//! into both endpoints that read `scope`, that it runs before anything is read or written,
//! and — the part most easily lost in a later refactor — that a token with no product claim
//! still reaches everything it did before.

use axum::extract::{Query, State};
use axum::Extension;
use chrono::Utc;
use sqlx::PgPool;

use crate::auth::product::Product;
use crate::auth::tokens::Claims;
use crate::routes::sync::status::{sync_status_handler, SyncStatusQuery};
use crate::routes::sync::tests::helpers::{seed_device, setup_state};
use crate::routes::sync::{
    parse_or_hash_uuid, sync_handler, AppError, AppJson, SyncRequest, SyncScope,
};

fn claims_for(product: Option<Product>) -> Claims {
    Claims {
        sub: "user-1".to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10_000_000_000,
        product,
    }
}

fn empty_request(scope: Option<SyncScope>) -> SyncRequest {
    SyncRequest {
        last_synced_at: None,
        client_id: "client-1".to_string(),
        device_uuid: None,
        device_name: None,
        scope,
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
    }
}

fn assert_forbidden(result: Result<impl std::fmt::Debug, AppError>) {
    match result {
        Err(AppError::Forbidden(_)) => {}
        other => panic!("expected a 403 for a cross-product scope, got {other:?}"),
    }
}

/// The case the second hostname makes reachable: a ScribbleRoute token asking for the
/// household's groceries.
#[sqlx::test]
async fn a_scribbleroute_token_cannot_sync_grocery(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_device(&pool, parse_or_hash_uuid("user-1"), "Tablet").await;

    let result = sync_handler(
        State(state),
        Extension(claims_for(Some(Product::ScribbleRoute))),
        AppJson(empty_request(Some(SyncScope::Grocery))),
    )
    .await;

    assert_forbidden(result);
}

/// And the other direction, which is the one that actually costs money: teddy.fyi's token
/// reaching into the drawings of a product with external testers.
#[sqlx::test]
async fn a_teddy_fyi_token_cannot_sync_drawings(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_device(&pool, parse_or_hash_uuid("user-1"), "Tablet").await;

    let result = sync_handler(
        State(state),
        Extension(claims_for(Some(Product::TeddyFyi))),
        AppJson(empty_request(Some(SyncScope::ScribbleKeepCloud))),
    )
    .await;

    assert_forbidden(result);
}

/// Omitting `scope` defaults to `All`, which is teddy.fyi's — so leaving the field out is
/// not a way around the check.
#[sqlx::test]
async fn a_scribbleroute_token_cannot_reach_all_by_omitting_the_scope(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_device(&pool, parse_or_hash_uuid("user-1"), "Tablet").await;

    let result = sync_handler(
        State(state),
        Extension(claims_for(Some(Product::ScribbleRoute))),
        AppJson(empty_request(None)),
    )
    .await;

    assert_forbidden(result);
}

#[sqlx::test]
async fn a_token_syncs_its_own_products_scope(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_device(&pool, parse_or_hash_uuid("user-1"), "Tablet").await;

    let response = sync_handler(
        State(state),
        Extension(claims_for(Some(Product::TeddyFyi))),
        AppJson(empty_request(Some(SyncScope::Grocery))),
    )
    .await;

    assert!(response.is_ok(), "a grocery token was refused its own scope");
}

/// The rollout guarantee. Every device holding a token minted before the claim existed —
/// and every device whose client ID is still unclassified — keeps working, on both sides.
/// This test failing is what a premature stage 3 looks like; see `crate::auth::product`.
#[sqlx::test]
async fn a_token_without_a_product_claim_still_reaches_both_products(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_device(&pool, parse_or_hash_uuid("user-1"), "Tablet").await;

    for scope in [SyncScope::Grocery, SyncScope::ScribbleKeepCloud] {
        let response = sync_handler(
            State(state.clone()),
            Extension(claims_for(None)),
            AppJson(empty_request(Some(scope))),
        )
        .await;

        assert!(
            response.is_ok(),
            "{scope:?} was refused for a token with no product claim"
        );
    }
}

/// `GET /api/sync/status` reads the same body-supplied scope and had the same hole.
#[sqlx::test]
async fn the_status_endpoint_enforces_the_same_boundary(pool: PgPool) {
    let state = setup_state(pool.clone());

    let refused = sync_status_handler(
        State(state.clone()),
        Extension(claims_for(Some(Product::ScribbleRoute))),
        Query(SyncStatusQuery {
            scope: Some(SyncScope::Grocery),
            last_synced_at: Some(Utc::now()),
        }),
    )
    .await;
    assert_forbidden(refused);

    let allowed = sync_status_handler(
        State(state),
        Extension(claims_for(Some(Product::ScribbleRoute))),
        Query(SyncStatusQuery {
            scope: Some(SyncScope::ScribbleKeep),
            last_synced_at: Some(Utc::now()),
        }),
    )
    .await;
    assert!(allowed.is_ok(), "a ScribbleRoute token was refused its own status scope");
}
