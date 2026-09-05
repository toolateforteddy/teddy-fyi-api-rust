//! The sweep only ever removes rows nothing can read again.
//!
//! Pinned because this sweep is the *only* bound on `device_claim_failures` growth for
//! accounts that are never deleted: account erasure (`routes::user::deletion`) clears the
//! rows of the account it erases, and everything else is on the clock below.

use super::*;

/// Expired and long-consumed pairing rows go, as do stale failure counters; a live code
/// and a fresh failure stay. The second half is the one worth pinning: a sweep that
/// dropped fresh failure rows would quietly disable the claim rate limit that reads them.
#[sqlx::test]
async fn only_dead_rows_are_swept(pool: PgPool) {
    for (hash, code, expires_at, consumed_at) in [
        // Live: still pollable.
        ("hash-live", "LIVECD01", Utc::now() + Duration::hours(1), None),
        // Expired, but inside the retention window: still answerable in support.
        (
            "hash-recent",
            "RECNTC01",
            Utc::now() - Duration::hours(1),
            None,
        ),
        // Expired long enough ago that nothing will ever read it again.
        (
            "hash-dead",
            "DEADCD01",
            Utc::now() - Duration::hours(RETENTION_HOURS + 1),
            None,
        ),
        // Consumed and spent, even though its own expiry has not arrived yet.
        (
            "hash-spent",
            "SPENTC01",
            Utc::now() + Duration::hours(1),
            Some(Utc::now() - Duration::hours(RETENTION_HOURS + 1)),
        ),
    ] {
        sqlx::query!(
            "INSERT INTO device_authorizations
                 (device_code_hash, user_code, client_uuid, expires_at, consumed_at)
             VALUES ($1, $2, 'client-1', $3, $4)",
            hash,
            code,
            expires_at,
            consumed_at
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    for failed_at in [
        Utc::now(),
        Utc::now() - Duration::hours(FAILURE_RETENTION_HOURS + 1),
    ] {
        sqlx::query!(
            "INSERT INTO device_claim_failures (user_id, failed_at) VALUES ('guesser', $1)",
            failed_at
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    let summary = reap_device_authorizations(&pool).await.unwrap();
    assert_eq!(
        summary,
        DeviceReapSummary {
            authorizations_deleted: 2,
            claim_failures_deleted: 1,
        }
    );

    let remaining: Vec<String> =
        sqlx::query_scalar!("SELECT user_code FROM device_authorizations ORDER BY user_code")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        remaining,
        vec!["LIVECD01".to_string(), "RECNTC01".to_string()]
    );
}
