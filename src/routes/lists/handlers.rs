//! Grocery-list sharing: minting an invite code, and redeeming one.
//!
//! Redeeming a code grants role `MEMBER` on somebody else's list — read *and* write of
//! their data — so `/api/lists/join` is a credential check that happens to look like a
//! form submission, and it is treated as one here. The numbers behind both handlers, and
//! the reasoning for each, live in [`crate::routes::lists::limits`].

use axum::{
    extract::State,
    Extension,
    Json,
};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::auth::tokens::Claims;
use crate::routes::lists::limits;
use crate::routes::sync::types::AppError;
use rand::RngExt;
use rand::distr::Alphanumeric;
use chrono::Utc;

/// The single answer every failed join gets.
///
/// Wrong code, expired code, code destroyed for being guessed at: one string, one status.
/// The handler used to distinguish "Invalid invite code" from "Expired invite code", which
/// told a guesser the one thing they most want to know — that the code they just tried
/// *exists* — and turned an 8-character search into a two-phase one. There is nothing a
/// real user can do with the distinction either: in both cases they need a new code.
const JOIN_FAILURE_MESSAGE: &str = "Invalid or expired invite code";

#[derive(Deserialize)]
pub struct InviteRequest {
    #[serde(alias = "list_id", rename = "listId")]
    pub list_id: String,
}

#[derive(Debug, Serialize)]
pub struct InviteResponse {
    pub code: String,
}

#[derive(Deserialize)]
pub struct JoinRequest {
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct JoinResponse {
    pub success: bool,
    #[serde(rename = "listId")]
    pub list_id: String,
}

/// `POST /api/lists/invite` — mint a code for a list the caller belongs to, superseding
/// whatever code that list had before.
///
/// A list has one live code at a time and it is good for
/// [`limits::invite_ttl_mins`] minutes. Refused with `429` once the caller already holds
/// [`limits::max_outstanding_invites_per_user`] live codes across their other lists.
pub async fn invite_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(payload): Json<InviteRequest>,
) -> Result<Json<InviteResponse>, AppError> {
    let user_id = &claims.sub;
    let list_id = &payload.list_id;

    // One transaction for the whole mint. Superseding the old code and inserting the new
    // one have to land together: a commit between them would leave the list either with
    // two live codes or with none.
    let mut tx = state.db_pool.begin().await?;

    // 1. Verify that the requesting user is a member of the grocery list
    let is_member = sqlx::query!(
        r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
        list_id,
        user_id
    )
    .fetch_optional(&mut *tx)
    .await?
    .is_some();

    if !is_member {
        return Err(AppError::Forbidden(format!(
            "User is not a member of grocery list {}",
            list_id
        )));
    }

    // 2. Serialise mints for this list against each other.
    //
    // The supersede below is a `DELETE`, and a `DELETE` cannot see a row another
    // transaction has inserted but not committed. Two people pressing "invite" on the same
    // list at the same moment would each delete nothing and each insert a code, and the
    // list would end up with the two live codes this endpoint exists to prevent. The lock
    // is held to the end of the transaction and is scoped to the list, so it costs
    // concurrent invites for *other* lists nothing.
    //
    // `list_invites` also carries a unique index on "listId", which makes one-code-per-list
    // a fact about the table rather than a property of this function; the lock is what
    // turns the race into a short wait instead of an error.
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtext($1)::bigint)",
        list_id
    )
    .execute(&mut *tx)
    .await?;

    // 3. Retire whatever code this list already had.
    //
    // Minting supersedes. A second code was never a second way in for the family — they
    // send one code to one person — but it was a second way in for everybody else, live
    // for as long as the first and just as good. Pressing "invite" again because the first
    // code was mistyped, or because it went to the wrong person, now takes the old one out
    // of circulation, which is what a person pressing that button already believes it does.
    sqlx::query!(r#"DELETE FROM list_invites WHERE "listId" = $1"#, list_id)
        .execute(&mut *tx)
        .await?;

    // 4. Refuse an account that is already sitting on its allowance of live codes.
    //
    // Only unexpired rows count. A redeemed invite is deleted outright and an expired one
    // can no longer grant anything, so neither is part of the surface this cap bounds —
    // and a parent whose code lapsed unused must be able to issue another immediately.
    // Counted after the supersede above, so re-inviting to a list the caller already has a
    // code for is never what trips the cap: that mint adds nothing to the surface.
    let outstanding = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!"
             FROM list_invites
            WHERE "createdBy" = $1 AND "expiresAt" > now()"#,
        user_id
    )
    .fetch_one(&mut *tx)
    .await?;

    if outstanding >= limits::max_outstanding_invites_per_user() {
        tracing::warn!(
            outstanding,
            "Invite refused: outstanding invite cap reached"
        );
        return Err(AppError::TooManyRequests(
            "Too many outstanding invites; wait for one to expire or be used".to_string(),
        ));
    }

    // 5. Draw a unique code and store it.
    //
    // A bounded number of draws, and the uniqueness check is the insert itself rather than
    // a `SELECT` before it. The old shape — look for a collision, then insert — was both
    // unbounded (a `loop` with no exit but success) and racy: two concurrent invites could
    // both see the code free and the second would fail on the primary key. `ON CONFLICT DO
    // NOTHING RETURNING` collapses both into one atomic attempt, the same way
    // `auth::device::start_handler` does it.
    let expires_at = Utc::now() + chrono::Duration::minutes(limits::invite_ttl_mins());

    for _ in 0..limits::INVITE_CODE_GENERATION_ATTEMPTS {
        let candidate: String = rand::rng()
            .sample_iter(Alphanumeric)
            .filter(|c| c.is_ascii_alphanumeric())
            .take(8)
            .map(|c| (c as char).to_ascii_uppercase())
            .collect();

        // An expired row still holds the primary key, so retire it before treating the
        // draw as a collision. Without this the code space shrinks monotonically until the
        // reaper runs.
        sqlx::query!(
            r#"DELETE FROM list_invites WHERE code = $1 AND "expiresAt" < now()"#,
            candidate
        )
        .execute(&mut *tx)
        .await?;

        let inserted = sqlx::query!(
            r#"INSERT INTO list_invites (code, "listId", "createdBy", "expiresAt")
               VALUES ($1, $2, $3, $4)
               ON CONFLICT DO NOTHING
               RETURNING code"#,
            candidate,
            list_id,
            user_id,
            expires_at
        )
        .fetch_optional(&mut *tx)
        .await?;

        if inserted.is_some() {
            tx.commit().await?;
            return Ok(Json(InviteResponse { code: candidate }));
        }
    }

    // Eight independent draws from a ~2.8e12 space all colliding is not bad luck; it is a
    // broken table or a broken generator. Say so as a 500 rather than spinning.
    tracing::error!("Exhausted invite code generation attempts");
    Err(AppError::Internal(
        "Could not allocate an invite code".to_string(),
    ))
}

/// `POST /api/lists/join` — redeem a code for membership of the list that issued it.
///
/// Every failure is [`JOIN_FAILURE_MESSAGE`] with a `403`, and an account that has failed
/// [`limits::max_join_failures`] times inside the window is refused with `429` before the
/// code is even looked up.
pub async fn join_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(payload): Json<JoinRequest>,
) -> Result<Json<JoinResponse>, AppError> {
    let user_id = &claims.sub;
    let code = payload.code.trim().to_ascii_uppercase();

    // The rate limit is checked before the lookup, so a locked-out account learns nothing
    // about the code it just sent — not even how long the query took.
    if join_failures_exhausted(&state, user_id).await? {
        tracing::warn!("Join refused: failed-attempt limit reached");
        return Err(AppError::TooManyRequests(
            "Too many invalid invite codes; try again later".to_string(),
        ));
    }

    let mut tx = state.db_pool.begin().await?;

    // `FOR UPDATE` locks the row for the rest of the transaction: two callers racing one
    // code cannot both spend it, and the attempt counter below cannot be lost to an
    // interleaved write.
    let invite = sqlx::query!(
        r#"SELECT "listId" as list_id, "expiresAt" as expires_at, attempts
             FROM list_invites WHERE code = $1 FOR UPDATE"#,
        code
    )
    .fetch_optional(&mut *tx)
    .await?;

    let Some(invite) = invite else {
        // No row to count against — this is the case the per-account counter exists for,
        // and it is the overwhelmingly common one when somebody is guessing.
        tx.rollback().await?;
        record_join_failure(&state, user_id).await;
        return Err(AppError::Forbidden(JOIN_FAILURE_MESSAGE.to_string()));
    };

    if invite.expires_at < Utc::now() {
        // A code that exists but is refused is one somebody may be probing. Count it
        // against the row, and destroy the row once it has been probed enough — an expired
        // invite is worth nothing to its owner and should not linger as a target.
        //
        // The row is deleted either way once the counter is spent; below that it is kept
        // so the counter itself survives, which is what makes the limit mean anything.
        let attempts = invite.attempts + 1;
        if attempts >= limits::max_invite_attempts() {
            sqlx::query!("DELETE FROM list_invites WHERE code = $1", code)
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query!(
                "UPDATE list_invites SET attempts = $2 WHERE code = $1",
                code,
                attempts
            )
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;

        record_join_failure(&state, user_id).await;
        // Identical to the unknown-code answer above, deliberately: see
        // [`JOIN_FAILURE_MESSAGE`].
        return Err(AppError::Forbidden(JOIN_FAILURE_MESSAGE.to_string()));
    }

    let list_id = invite.list_id;

    // Create or re-activate list membership for the caller.
    let member_id = format!("{}-member-{}", list_id, user_id);
    let joined_at = Utc::now().timestamp_millis();

    sqlx::query!(
        r#"INSERT INTO grocery_list_members (
            id, "listId", "userId", role, "joinedAt", version, is_deleted, sync_state, updated_at, updated_by_client
        ) VALUES ($1, $2, $3, $4, $5, 1, FALSE, 'SYNCED', NOW(), NULL)
        ON CONFLICT (id) DO UPDATE SET
            is_deleted = FALSE,
            version = grocery_list_members.version + 1,
            updated_at = NOW(),
            updated_by_client = NULL"#,
        member_id,
        list_id,
        user_id,
        "MEMBER",
        joined_at
    )
    .execute(&mut *tx)
    .await?;

    // Single-use: the code dies with the redemption that used it.
    sqlx::query!("DELETE FROM list_invites WHERE code = $1", code)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(Json(JoinResponse {
        success: true,
        list_id,
    }))
}

/// True once this account has failed [`limits::max_join_failures`] joins inside the
/// window.
///
/// Counted per account rather than per code, for the same reason
/// `claim_failures_exhausted` is: a guess that matches nothing leaves no code to count
/// against, and guessing is precisely the behaviour being limited.
async fn join_failures_exhausted(state: &AppState, user_id: &str) -> Result<bool, AppError> {
    let since = Utc::now() - chrono::Duration::minutes(limits::join_failure_window_mins());
    let failures = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!"
             FROM list_join_failures
            WHERE user_id = $1 AND failed_at > $2"#,
        user_id,
        since
    )
    .fetch_one(&state.db_pool)
    .await?;

    Ok(failures >= limits::max_join_failures())
}

/// Records one failed join against the account.
///
/// Best-effort, exactly as `record_claim_failure` is: a bookkeeping failure must not turn
/// a `403` into a `500`, because the difference between the two is itself a signal about
/// the code that was sent. Written on its own pool connection rather than in the caller's
/// transaction, so that a failure it records survives the rollback of the attempt.
async fn record_join_failure(state: &AppState, user_id: &str) {
    if let Err(e) = sqlx::query!(
        "INSERT INTO list_join_failures (user_id) VALUES ($1)",
        user_id
    )
    .execute(&state.db_pool)
    .await
    {
        tracing::error!("Failed to record list join failure: {:?}", e);
    }
}

#[cfg(test)]
mod tests;
