//! The per-account device cap, from the endpoint that a real tablet calls.
//!
//! Two things are being asserted, and the second matters more than the first: that an
//! account cannot register devices without bound, and that a family already at (or past)
//! the cap can still use the tablets they own. A cap that locks a real household out of
//! its own hardware is worse than the abuse it prevents.

use super::*;
use crate::routes::devices::limits;
use crate::routes::sync::tests::helpers::{seed_device, setup_state};
use axum::response::IntoResponse;
use axum::http::StatusCode;
use sqlx::PgPool;

fn claims(user_id: &str) -> Claims {
    Claims {
        sub: user_id.to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10_000_000_000,
    }
}

async fn register(
    state: &AppState,
    user_id: &str,
    device_uuid: Option<Uuid>,
    name: Option<&str>,
) -> Result<DeviceResponse, AppError> {
    register_device_handler(
        State(state.clone()),
        Extension(claims(user_id)),
        Json(RegisterDeviceRequest {
            device_uuid,
            name: name.map(str::to_string),
        }),
    )
    .await
    .map(|response| response.0)
}

async fn status_of(err: AppError) -> StatusCode {
    err.into_response().status()
}

async fn device_count(pool: &PgPool, user_id: &str) -> i64 {
    let user_uuid = crate::routes::sync::remote_mutations::parse_or_hash_uuid(user_id);
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM devices WHERE user_id = $1"#,
        user_uuid
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Fills the account to exactly the cap and hands back the ids, so a test can then ask
/// what happens at the boundary.
async fn fill_to_cap(state: &AppState, user_id: &str) -> Vec<Uuid> {
    let mut ids = Vec::new();
    for n in 0..limits::max_devices_per_account() {
        let device = register(state, user_id, Some(Uuid::new_v4()), Some("Tablet"))
            .await
            .unwrap_or_else(|_| panic!("device {} is under the cap and must register", n));
        ids.push(device.id);
    }
    ids
}

/// Under the cap every registration succeeds; the one past it is a 429, and it creates no
/// row.
#[sqlx::test]
async fn registration_is_capped_per_account(pool: PgPool) {
    let state = setup_state(pool.clone());
    let ids = fill_to_cap(&state, "user-1").await;
    assert_eq!(device_count(&pool, "user-1").await, ids.len() as i64);

    let refused = register(&state, "user-1", Some(Uuid::new_v4()), Some("Eleventh"))
        .await
        .unwrap_err();
    assert_eq!(status_of(refused).await, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(device_count(&pool, "user-1").await, ids.len() as i64);

    // The cap is per account. Another family is not affected by this one's behaviour.
    assert!(register(&state, "user-2", Some(Uuid::new_v4()), Some("Tablet"))
        .await
        .is_ok());
}

/// The regression this cap most easily causes: a tablet the family already owns must keep
/// working once the account is full.
///
/// A real tablet re-registers on every launch — that is how it reports its name and gets
/// its id confirmed — so if the cap were checked before the "already yours" branch, filling
/// an account would brick every device on it at once.
#[sqlx::test]
async fn an_existing_device_can_always_re_register(pool: PgPool) {
    let state = setup_state(pool.clone());
    let ids = fill_to_cap(&state, "user-1").await;

    // At the cap.
    let again = register(&state, "user-1", Some(ids[0]), Some("Kitchen tablet"))
        .await
        .expect("a device the account already owns must re-register at the cap");
    assert_eq!(again.id, ids[0]);
    assert_eq!(again.name, "Kitchen tablet");

    // And past it: an account that was over the cap before the cap existed, or after the
    // number is lowered, must not lose access to the tablets it has.
    sqlx::query!(
        "INSERT INTO devices (id, user_id, name) VALUES ($1, $2, $3)",
        Uuid::new_v4(),
        crate::routes::sync::remote_mutations::parse_or_hash_uuid("user-1"),
        "Grandfathered"
    )
    .execute(&pool)
    .await
    .unwrap();

    assert!(register(&state, "user-1", Some(ids[1]), Some("Playroom"))
        .await
        .is_ok());
    // Still no *new* device, though.
    assert_eq!(
        status_of(
            register(&state, "user-1", Some(Uuid::new_v4()), None)
                .await
                .unwrap_err()
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

/// A device belonging to somebody else is still a 403, not a 429. The two mean different
/// things — "that is not yours" versus "you have no room" — and only one of them is fixed
/// by deleting a device.
#[sqlx::test]
async fn renaming_device_with_empty_name_returns_bad_request(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "BouncyMeadowAdventure").await;

    let err = rename_device_handler(
        State(state),
        Extension(claims("user-1")),
        Path(device_uuid),
        Json(RenameDeviceRequest {
            name: "   ".to_string(),
        }),
    )
    .await
    .unwrap_err();

    assert_eq!(status_of(err).await, StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn another_accounts_device_is_still_forbidden(pool: PgPool) {
    let state = setup_state(pool.clone());
    let theirs = register(&state, "user-2", Some(Uuid::new_v4()), Some("Theirs"))
        .await
        .unwrap();

    let err = register(&state, "user-1", Some(theirs.id), Some("Mine now"))
        .await
        .unwrap_err();
    assert_eq!(status_of(err).await, StatusCode::FORBIDDEN);
}

/// Removing a device frees the slot it held, which is what makes the 429 recoverable
/// without support intervention.
#[sqlx::test]
async fn deleting_a_device_frees_a_slot(pool: PgPool) {
    let state = setup_state(pool.clone());
    let ids = fill_to_cap(&state, "user-1").await;

    sqlx::query!("DELETE FROM devices WHERE id = $1", ids[0])
        .execute(&pool)
        .await
        .unwrap();

    assert!(register(&state, "user-1", Some(Uuid::new_v4()), Some("Replacement"))
        .await
        .is_ok());
}

/// With nothing set in the environment the cap is the compiled default, and the constant
/// is where the argument for the number lives.
#[test]
fn the_cap_defaults_when_the_environment_is_silent() {
    assert_eq!(
        limits::max_devices_per_account(),
        limits::DEFAULT_MAX_DEVICES_PER_ACCOUNT
    );
}
