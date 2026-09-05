//! Invite minting and code redemption, exercised against a real database.
//!
//! The interesting properties here are all negative ones — what an attacker cannot learn,
//! and what they cannot make the service do — so most of these tests assert about
//! *failures* rather than successes.

use super::*;
use crate::routes::sync::tests::helpers::setup_state;
use axum::response::IntoResponse;
use axum::http::StatusCode;
use sqlx::PgPool;

/// Seeds a list and makes `owner` a member of it, which is the precondition
/// `invite_handler` checks.
async fn seed_list(pool: &PgPool, list_id: &str, owner: &str) {
    sqlx::query!(
        r#"INSERT INTO grocery_lists (id, name, "ownerId", "createdAt") VALUES ($1, $2, $3, 0)"#,
        list_id,
        "Weekly shop",
        owner
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        r#"INSERT INTO grocery_list_members (id, "listId", "userId", role, "joinedAt")
           VALUES ($1, $2, $3, 'OWNER', 0)"#,
        format!("{}-member-{}", list_id, owner),
        list_id,
        owner
    )
    .execute(pool)
    .await
    .unwrap();
}

fn claims(user_id: &str) -> Claims {
    Claims {
        sub: user_id.to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10_000_000_000,
    }
}

async fn invite(state: &AppState, user_id: &str, list_id: &str) -> Result<String, AppError> {
    invite_handler(
        State(state.clone()),
        Extension(claims(user_id)),
        Json(InviteRequest {
            list_id: list_id.to_string(),
        }),
    )
    .await
    .map(|response| response.0.code)
}

async fn join(state: &AppState, user_id: &str, code: &str) -> Result<JoinResponse, AppError> {
    join_handler(
        State(state.clone()),
        Extension(claims(user_id)),
        Json(JoinRequest {
            code: code.to_string(),
        }),
    )
    .await
    .map(|response| response.0)
}

/// An `AppError` as the caller actually sees it: the status and the body's `error` string.
/// The point of most of these assertions is that two different internal situations produce
/// exactly the same pair.
async fn seen_by_caller(err: AppError) -> (StatusCode, String) {
    let response = err.into_response();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    (status, body["error"].as_str().unwrap().to_string())
}

/// Backdates an invite past its expiry without waiting 24 hours.
async fn expire_invite(pool: &PgPool, code: &str) {
    sqlx::query!(
        r#"UPDATE list_invites SET "expiresAt" = now() - interval '1 hour' WHERE code = $1"#,
        code
    )
    .execute(pool)
    .await
    .unwrap();
}

async fn outstanding_invites(pool: &PgPool, user_id: &str) -> i64 {
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM list_invites WHERE "createdBy" = $1"#,
        user_id
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The flow the feature exists for, unchanged by any of the limits.
#[sqlx::test]
async fn a_member_can_invite_and_a_stranger_can_join(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_list(&pool, "list-1", "owner-1").await;

    let code = invite(&state, "owner-1", "list-1").await.unwrap();
    assert_eq!(code.len(), 8);

    let joined = join(&state, "guest-1", &code).await.unwrap();
    assert_eq!(joined.list_id, "list-1");

    // Single use: the code is gone, and presenting it again is an ordinary failure.
    assert_eq!(outstanding_invites(&pool, "owner-1").await, 0);
    assert!(join(&state, "guest-2", &code).await.is_err());
}

/// The core of the finding: a wrong code and an expired code must be one answer.
///
/// If they differ, a guesser who stumbles on a real-but-expired code is told so, and the
/// 8-character search collapses into "find any code, then wait for a fresh one on the same
/// list" — which is a much cheaper problem.
#[sqlx::test]
async fn wrong_and_expired_codes_are_indistinguishable(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_list(&pool, "list-1", "owner-1").await;

    let code = invite(&state, "owner-1", "list-1").await.unwrap();
    expire_invite(&pool, &code).await;

    let expired = seen_by_caller(join(&state, "guest-1", &code).await.unwrap_err()).await;
    // A code that has never existed. Different guest, so the per-account counter (which is
    // itself observable) cannot be what makes the two answers agree.
    let unknown = seen_by_caller(join(&state, "guest-2", "ZZZZZZZZ").await.unwrap_err()).await;

    assert_eq!(expired, unknown);
    assert_eq!(expired.0, StatusCode::FORBIDDEN);
    assert_eq!(expired.1, JOIN_FAILURE_MESSAGE);
}

/// Wrong guesses cost the guesser their next guesses.
#[sqlx::test]
async fn repeated_bad_codes_lock_the_account_out(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_list(&pool, "list-1", "owner-1").await;
    let real_code = invite(&state, "owner-1", "list-1").await.unwrap();

    for attempt in 0..limits::max_join_failures() {
        let (status, _) = seen_by_caller(join(&state, "guesser", "AAAAAAAA").await.unwrap_err())
            .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "attempt {} should still be an ordinary refusal",
            attempt
        );
    }

    // Past the limit the account is refused before the code is looked up — which is why
    // even the *correct* code no longer works for it.
    let (status, _) = seen_by_caller(join(&state, "guesser", &real_code).await.unwrap_err()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // And the lockout is per account: an honest user is not caught in it.
    assert_eq!(join(&state, "guest-1", &real_code).await.unwrap().list_id, "list-1");
}

/// The per-row counter: a code being probed after it lapsed is destroyed rather than left
/// standing as a target for the rest of the retention window.
#[sqlx::test]
async fn a_probed_expired_code_is_destroyed(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_list(&pool, "list-1", "owner-1").await;
    let code = invite(&state, "owner-1", "list-1").await.unwrap();
    expire_invite(&pool, &code).await;

    // A distinct guesser per attempt, so the per-account limit above cannot be what ends
    // the loop — this test is about the row, not the account.
    for attempt in 0..limits::max_invite_attempts() {
        assert_eq!(
            outstanding_invites(&pool, "owner-1").await,
            1,
            "the row should survive until the counter is spent (attempt {})",
            attempt
        );
        assert!(join(&state, &format!("guesser-{}", attempt), &code)
            .await
            .is_err());
    }

    assert_eq!(outstanding_invites(&pool, "owner-1").await, 0);
}

/// Under the cap, at the cap, and the shape of the refusal.
#[sqlx::test]
async fn outstanding_invites_are_capped(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_list(&pool, "list-1", "owner-1").await;

    let mut codes = Vec::new();
    for n in 0..limits::max_outstanding_invites_per_user() {
        codes.push(
            invite(&state, "owner-1", "list-1")
                .await
                .unwrap_or_else(|_| panic!("invite {} is under the cap and must succeed", n)),
        );
    }

    let (status, _) = seen_by_caller(invite(&state, "owner-1", "list-1").await.unwrap_err()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);

    // The cap is per account, not global: a different parent is unaffected.
    seed_list(&pool, "list-2", "owner-2").await;
    assert!(invite(&state, "owner-2", "list-2").await.is_ok());

    // A redeemed invite stops counting, so the account gets its slot back.
    join(&state, "guest-1", &codes[0]).await.unwrap();
    assert!(invite(&state, "owner-1", "list-1").await.is_ok());

    // So does an expired one — a parent whose code lapsed unused is not penalised.
    expire_invite(&pool, &codes[1]).await;
    assert!(invite(&state, "owner-1", "list-1").await.is_ok());
}

/// The membership check is unchanged, and it is not a 429: being refused a list you do not
/// belong to has nothing to do with quotas.
#[sqlx::test]
async fn a_non_member_cannot_invite(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_list(&pool, "list-1", "owner-1").await;

    let (status, _) = seen_by_caller(invite(&state, "stranger", "list-1").await.unwrap_err()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// Codes are matched case- and whitespace-insensitively, as they were before: a parent
/// reading one off a phone screen must not be punished for typing it in lower case.
#[sqlx::test]
async fn codes_are_normalised_before_lookup(pool: PgPool) {
    let state = setup_state(pool.clone());
    seed_list(&pool, "list-1", "owner-1").await;
    let code = invite(&state, "owner-1", "list-1").await.unwrap();

    let joined = join(&state, "guest-1", &format!("  {}  ", code.to_lowercase()))
        .await
        .unwrap();
    assert_eq!(joined.list_id, "list-1");
}

/// With nothing set in the environment, every limit is the compiled default.
///
/// Pins the accessors to the constants the rest of these tests reason about — a future
/// edit that changes a number has to change the constant, where the argument for it is
/// written down, rather than only the reader. (The parse-and-reject-nonsense half is not
/// exercised here: mutating process environment from a test is racy across the shared test
/// binary and `unsafe` besides.)
#[test]
fn limits_default_when_the_environment_is_silent() {
    assert_eq!(limits::max_join_failures(), limits::DEFAULT_MAX_JOIN_FAILURES);
    assert_eq!(
        limits::max_outstanding_invites_per_user(),
        limits::DEFAULT_MAX_OUTSTANDING_INVITES_PER_USER
    );
    assert_eq!(
        limits::max_invite_attempts(),
        limits::DEFAULT_MAX_INVITE_ATTEMPTS
    );
    assert_eq!(
        limits::join_failure_window_mins(),
        limits::DEFAULT_JOIN_FAILURE_WINDOW_MINS
    );
}
