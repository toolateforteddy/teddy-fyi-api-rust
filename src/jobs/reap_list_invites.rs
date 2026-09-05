//! Drops grocery-list invite rows that can no longer do anything.
//!
//! Two reasons, and the second is the one that matters:
//!
//! * An expired invite grants nothing, but it still occupies its code in a primary-keyed
//!   8-character space. Left forever, the space a new invite can be drawn from shrinks
//!   monotonically. (`invite_handler` retires a single colliding expired row itself, so
//!   this is hygiene rather than the only defence.)
//! * The outstanding-invite cap counts only unexpired rows, and the failed-join limiter
//!   counts only rows inside its window — so everything outside those is pure storage that
//!   nothing will ever read again. A patch that exists to bound row growth should not
//!   itself leave a table growing without bound.
//!
//! Runs from the same `reap-stale-users` invocation the cluster's CronJob already makes;
//! see `main::run_reaper`, and `reap_device_authorizations` next to it, which this mirrors.

use chrono::{Duration, Utc};
use sqlx::PgPool;

use crate::routes::sync::types::AppError;

/// How long an expired invite is kept before deletion. A day of slack so that "it said my
/// code had expired" is still answerable from the table, and no longer, because an expired
/// code is worth nothing to anyone but a guesser.
const INVITE_RETENTION_HOURS: i64 = 24;

/// Failure counters older than this cannot affect any rate-limit window — the window is
/// ten minutes — so they are only rows. Same number the claim-failure sweep uses.
const FAILURE_RETENTION_HOURS: i64 = 24;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ListInviteReapSummary {
    pub invites_deleted: u64,
    pub join_failures_deleted: u64,
}

pub async fn reap_list_invites(pool: &PgPool) -> Result<ListInviteReapSummary, AppError> {
    let invites_deleted = sqlx::query!(
        r#"DELETE FROM list_invites WHERE "expiresAt" < $1"#,
        Utc::now() - Duration::hours(INVITE_RETENTION_HOURS)
    )
    .execute(pool)
    .await?
    .rows_affected();

    let join_failures_deleted = sqlx::query!(
        "DELETE FROM list_join_failures WHERE failed_at < $1",
        Utc::now() - Duration::hours(FAILURE_RETENTION_HOURS)
    )
    .execute(pool)
    .await?
    .rows_affected();

    Ok(ListInviteReapSummary {
        invites_deleted,
        join_failures_deleted,
    })
}

#[cfg(test)]
mod tests;
