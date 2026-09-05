//! Binding the requested `SyncScope` to something the caller actually proved.
//!
//! `scope` is a field in the request body. Until this module existed it was read straight
//! out of the payload and used to select which tables the request would read and write —
//! so any valid access token reached all six scopes and, with them, both products' data.
//! Nothing in the token said otherwise, because until [`Product`] there was nothing in a
//! token that distinguished the two products at all.
//!
//! This is the enforcement half of that claim. The rule is one line: **a token that names
//! a product may only ask for that product's scopes.**
//!
//! # Why `All` belongs to teddy.fyi
//!
//! It is not a wildcard. `sync_handler` enters the todo and grocery futures for `All` and
//! the ScribbleRoute branch only for the three `Scribble*` variants, so `All` already
//! *means* "the teddy.fyi scopes" everywhere it is read — including as the default when a
//! client omits `scope`, which is why every ScribbleRoute client already sends one
//! explicitly. Treating it as teddy.fyi's is a description of the existing behaviour, not
//! a new restriction on it.
//!
//! # Why an absent claim is permitted
//!
//! Denying it would sign out every device holding a token minted before this shipped, and
//! every device whose client ID is not classified per product yet — which today includes
//! everything arriving through the mixed `GOOGLE_IOS_CLIENT_IDS` secret. Both are
//! configuration and time, not caller behaviour, and neither is the caller's fault. So an
//! absent claim logs and proceeds, exactly as it did before this module existed, and the
//! boundary hardens on its own as sessions turn over and client IDs get classified. See
//! [`crate::auth::product`] for the three stages and what makes the last one safe.

use crate::auth::product::Product;
use crate::auth::tokens::Claims;
use crate::routes::sync::types::{AppError, SyncScope};

impl SyncScope {
    /// The product that owns this scope's tables.
    ///
    /// The mapping is the one the split plan's data-ownership table states: `Todo` and
    /// `Grocery` (and therefore `All`) are teddy.fyi's, the three `Scribble*` scopes are
    /// ScribbleRoute's. It is exhaustive on purpose — a new scope will not compile until
    /// somebody says which side of the split it lands on, which is the whole point of
    /// writing it down before there are two repositories to write it down in.
    pub fn product(self) -> Product {
        match self {
            SyncScope::All | SyncScope::Todo | SyncScope::Grocery => Product::TeddyFyi,
            SyncScope::ScribbleBox | SyncScope::ScribbleKeep | SyncScope::ScribbleKeepCloud => {
                Product::ScribbleRoute
            }
        }
    }
}

/// Fails the request unless the caller's token permits `scope`.
///
/// Called by both readers of `scope` — `POST /api/sync` and `GET /api/sync/status` — before
/// either opens a transaction or touches Redis, because a 403 that has already read a
/// table is not much of a boundary.
pub fn authorize_scope(claims: &Claims, scope: SyncScope) -> Result<(), AppError> {
    let Some(product) = claims.product else {
        // Unknown, not denied. Logged at debug rather than warn: during the rollout this is
        // the common case, and a line per sync request per device would drown the log it is
        // supposed to inform. The count that matters — how many client IDs are still
        // unclassified — is a single warn at start-up instead.
        tracing::debug!(
            scope = ?scope,
            "Sync scope permitted without a product claim: token predates the claim, or its \
             client ID is not classified per product"
        );
        return Ok(());
    };

    if product == scope.product() {
        return Ok(());
    }

    tracing::warn!(
        token_product = %product,
        scope = ?scope,
        scope_product = %scope.product(),
        "Sync scope refused: a token issued for one product asked for the other's scope"
    );
    Err(AppError::Forbidden(format!(
        "Scope {scope:?} does not belong to this token's product"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims_for(product: Option<Product>) -> Claims {
        Claims {
            sub: "user-1".to_string(),
            client_uuid: "client-1".to_string(),
            exp: 0,
            product,
        }
    }

    const TEDDY_SCOPES: [SyncScope; 3] = [SyncScope::All, SyncScope::Todo, SyncScope::Grocery];
    const SCRIBBLE_SCOPES: [SyncScope; 3] = [
        SyncScope::ScribbleBox,
        SyncScope::ScribbleKeep,
        SyncScope::ScribbleKeepCloud,
    ];

    #[test]
    fn a_token_reaches_its_own_products_scopes() {
        for scope in TEDDY_SCOPES {
            assert!(authorize_scope(&claims_for(Some(Product::TeddyFyi)), scope).is_ok());
        }
        for scope in SCRIBBLE_SCOPES {
            assert!(authorize_scope(&claims_for(Some(Product::ScribbleRoute)), scope).is_ok());
        }
    }

    /// The cross-product boundary itself, in both directions: this is what did not exist
    /// before, and what the second hostname makes reachable.
    #[test]
    fn a_token_cannot_reach_the_other_products_scopes() {
        for scope in SCRIBBLE_SCOPES {
            assert!(authorize_scope(&claims_for(Some(Product::TeddyFyi)), scope).is_err());
        }
        for scope in TEDDY_SCOPES {
            assert!(authorize_scope(&claims_for(Some(Product::ScribbleRoute)), scope).is_err());
        }
    }

    /// `All` is teddy.fyi's, so a ScribbleRoute token cannot get there by *omitting* the
    /// field either — the handler's `unwrap_or(All)` runs before this check, not around it.
    #[test]
    fn omitting_the_scope_does_not_widen_a_scribbleroute_token() {
        let scope = SyncScope::default();
        assert_eq!(scope, SyncScope::All);
        assert!(authorize_scope(&claims_for(Some(Product::ScribbleRoute)), scope).is_err());
    }

    /// The rollout property. Deleting this test is stage 3 of the plan in
    /// `crate::auth::product`, and must not happen before the sessions it protects are gone.
    #[test]
    fn a_token_with_no_product_claim_still_reaches_every_scope() {
        for scope in TEDDY_SCOPES.into_iter().chain(SCRIBBLE_SCOPES) {
            assert!(
                authorize_scope(&claims_for(None), scope).is_ok(),
                "{scope:?} was refused for a token with no product claim"
            );
        }
    }

    #[test]
    fn the_refusal_is_a_403_that_names_the_scope() {
        let err = authorize_scope(&claims_for(Some(Product::TeddyFyi)), SyncScope::ScribbleKeep)
            .unwrap_err();
        match err {
            AppError::Forbidden(message) => assert!(message.contains("ScribbleKeep")),
            other => panic!("expected Forbidden, got {other:?}"),
        }
    }
}
