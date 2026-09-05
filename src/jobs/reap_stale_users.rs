//! Deletes accounts that have gone 12 months without a sync.
//!
//! This implements the retention commitment published at <https://scribbleroute.com/privacy>,
//! which defines the clock in terms of the last time one of the account's *devices*
//! synced, restarting from the most recent sync. Two consequences drive the rules below:
//!
//! * **Activity is `MAX` over the account's devices**, not per device, and not a separate
//!   usage field — the same policy states nothing else about app usage is recorded.
//! * **Only ScribbleRoute accounts are in scope.** `users` is shared with the teddy.fyi
//!   grocery/todo side, whose clients never create a `devices` row (`touch_device` runs
//!   only in the ScribbleBox/ScribbleKeep branch of the sync handler), and which no
//!   published retention policy covers. An account with no devices is therefore *not
//!   eligible*, rather than infinitely stale.

use chrono::{DateTime, Months, Utc};
use sqlx::PgPool;
use std::collections::HashMap;
use uuid::Uuid;

use crate::routes::sync::publish_conn::RedisPublisher;
use crate::routes::sync::remote_mutations::parse_or_hash_uuid;
use crate::routes::sync::types::AppError;
use crate::routes::user::deletion::{announce_deletion, delete_user_data};

/// Months of sync silence before an account is deleted. The published policy says 12.
const DEFAULT_INACTIVE_MONTHS: u32 = 12;

pub struct ReapConfig {
    pub inactive_months: u32,
    /// When set, every account is erased inside a transaction that is then rolled back,
    /// so the log reports the real row counts without anything being lost. Defaults to
    /// `true`: arming the job takes an explicit `REAP_DRY_RUN=false`.
    pub dry_run: bool,
}

impl ReapConfig {
    pub fn from_env() -> Self {
        Self {
            inactive_months: parse_inactive_months(std::env::var("REAP_INACTIVE_MONTHS").ok()),
            dry_run: parse_dry_run(std::env::var("REAP_DRY_RUN").ok()),
        }
    }
}

/// Anything other than an explicit "false" leaves the job in dry-run, so a typo in the
/// manifest fails safe. Split out from [`ReapConfig::from_env`] so the rule is testable
/// without mutating process-wide environment state.
fn parse_dry_run(raw: Option<String>) -> bool {
    raw.map(|raw| !raw.trim().eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

/// An unset or unparseable window falls back to the 12 months the policy publishes.
fn parse_inactive_months(raw: Option<String>) -> u32 {
    raw.and_then(|raw| raw.trim().parse::<u32>().ok())
        .filter(|months| *months > 0)
        .unwrap_or(DEFAULT_INACTIVE_MONTHS)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReapSummary {
    /// Accounts that had at least one device, i.e. everything the sweep considered.
    pub scanned: usize,
    pub eligible: usize,
    /// Accounts actually erased. Equal to `eligible` on a clean run, and zero on a dry run.
    pub deleted: usize,
    pub failed: usize,
    pub dry_run: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct StaleUser {
    pub user_id: String,
    pub last_activity: DateTime<Utc>,
}

/// Accounts whose most recent device sync predates `cutoff`, plus the number of accounts
/// the sweep considered at all.
///
/// `devices.user_id` is the UUID derived from the auth subject by [`parse_or_hash_uuid`],
/// which is a one-way hash for non-UUID subjects, so the join back to `users.id` cannot be
/// expressed in SQL and is done here instead.
///
/// A device that has never synced falls back to its `created_at`. The device backfill in
/// migration `20260901120000` left `last_seen_at` NULL on every pre-existing device, and
/// reading those as "never synced" would make the entire install eligible at once; the
/// fallback instead starts their clock at the migration.
pub async fn find_stale_users(
    pool: &PgPool,
    cutoff: DateTime<Utc>,
) -> Result<(Vec<StaleUser>, usize), AppError> {
    let rows = sqlx::query!(
        r#"SELECT user_id AS "user_id!", MAX(COALESCE(last_seen_at, created_at)) AS "last_activity!"
           FROM devices
           GROUP BY user_id"#
    )
    .fetch_all(pool)
    .await?;

    let last_activity: HashMap<Uuid, DateTime<Utc>> = rows
        .into_iter()
        .map(|row| (row.user_id, row.last_activity))
        .collect();

    let user_ids: Vec<String> = sqlx::query_scalar!(r#"SELECT id FROM users"#)
        .fetch_all(pool)
        .await?;

    let mut scanned = 0;
    let mut stale = Vec::new();
    for user_id in user_ids {
        // No devices row: not a ScribbleRoute account, so out of scope entirely.
        let Some(&activity) = last_activity.get(&parse_or_hash_uuid(&user_id)) else {
            continue;
        };
        scanned += 1;
        if activity < cutoff {
            stale.push(StaleUser { user_id, last_activity: activity });
        }
    }

    Ok((stale, scanned))
}

/// One sweep: find stale accounts and delete each in its own transaction.
///
/// Per-account transactions mean one bad row cannot abort the rest of the sweep, and a
/// missed account is simply picked up by the next run.
pub async fn reap_stale_users(
    pool: &PgPool,
    publisher: &RedisPublisher,
    config: &ReapConfig,
) -> Result<ReapSummary, AppError> {
    let cutoff = Utc::now()
        .checked_sub_months(Months::new(config.inactive_months))
        .ok_or_else(|| AppError::Internal("Inactivity window overflows the calendar".to_string()))?;

    let (stale, scanned) = find_stale_users(pool, cutoff).await?;

    tracing::info!(
        cutoff = %cutoff,
        scanned,
        eligible = stale.len(),
        dry_run = config.dry_run,
        "Stale account sweep starting"
    );

    let mut summary = ReapSummary {
        scanned,
        eligible: stale.len(),
        dry_run: config.dry_run,
        ..Default::default()
    };

    for user in stale {
        match reap_one(pool, publisher, &user, config.dry_run).await {
            Ok(()) => {
                if !config.dry_run {
                    summary.deleted += 1;
                }
            }
            Err(err) => {
                summary.failed += 1;
                tracing::error!(
                    user_id = %user.user_id,
                    "Failed to delete stale account, leaving it for the next sweep: {:?}",
                    err
                );
            }
        }
    }

    tracing::info!(summary = ?summary, "Stale account sweep finished");
    Ok(summary)
}

/// Erases one account, or rolls the erase back when dry-running so the logged counts are
/// the ones a real run would produce.
async fn reap_one(
    pool: &PgPool,
    publisher: &RedisPublisher,
    user: &StaleUser,
    dry_run: bool,
) -> Result<(), AppError> {
    let mut tx = pool.begin().await?;
    let (deleted, affected_users) = delete_user_data(&mut tx, &user.user_id).await?;

    if dry_run {
        tx.rollback().await?;
        tracing::info!(
            user_id = %user.user_id,
            last_activity = %user.last_activity,
            would_delete = ?deleted,
            "DRY RUN: stale account left intact"
        );
        return Ok(());
    }

    tx.commit().await?;
    tracing::info!(
        user_id = %user.user_id,
        last_activity = %user.last_activity,
        deleted = ?deleted,
        "Deleted stale account"
    );

    announce_deletion(publisher, &user.user_id, &affected_users).await;
    Ok(())
}

#[cfg(test)]
mod tests;
