//! Drops device-pairing rows that can no longer do anything.
//!
//! Expired and spent codes are dead weight and mild risk — a `user_code` sits in the clear
//! by design, on the argument that it is short-lived, and that argument only holds if the
//! rows actually go away. Run from the same `reap-stale-users` invocation the cluster's
//! CronJob already makes; see `main::run_reaper`.

use chrono::{Duration, Utc};
use sqlx::PgPool;

use crate::routes::sync::types::AppError;

/// How long a dead row is kept before it is deleted. Long enough that a parent reporting
/// "it said the code expired" can still be looked up, short enough to be meaningless to an
/// attacker: the code stopped working the moment it expired.
const RETENTION_HOURS: i64 = 24;

/// Failure counters older than this cannot affect any rate-limit window, so they are only
/// rows. The claim window is ten minutes; a day of slack costs nothing.
const FAILURE_RETENTION_HOURS: i64 = 24;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct DeviceReapSummary {
    pub authorizations_deleted: u64,
    pub claim_failures_deleted: u64,
}

pub async fn reap_device_authorizations(pool: &PgPool) -> Result<DeviceReapSummary, AppError> {
    let cutoff = Utc::now() - Duration::hours(RETENTION_HOURS);

    // Consumed codes are as dead as expired ones, but their own `expires_at` may still be
    // in the future, so both clocks are checked.
    let authorizations_deleted = sqlx::query!(
        "DELETE FROM device_authorizations
          WHERE expires_at < $1
             OR (consumed_at IS NOT NULL AND consumed_at < $1)",
        cutoff
    )
    .execute(pool)
    .await?
    .rows_affected();

    let claim_failures_deleted = sqlx::query!(
        "DELETE FROM device_claim_failures WHERE failed_at < $1",
        Utc::now() - Duration::hours(FAILURE_RETENTION_HOURS)
    )
    .execute(pool)
    .await?
    .rows_affected();

    Ok(DeviceReapSummary {
        authorizations_deleted,
        claim_failures_deleted,
    })
}

#[cfg(test)]
mod tests;
