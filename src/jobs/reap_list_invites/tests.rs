//! The sweep only ever removes rows nothing can read again.

use super::*;

/// Long-expired invites and stale failure counters go; a live invite and a recent failure
/// stay. The second half is the one worth pinning: a sweep that dropped fresh failure rows
/// would quietly disable the brute-force limit that depends on them.
#[sqlx::test]
async fn only_dead_rows_are_swept(pool: PgPool) {
    sqlx::query!(
        r#"INSERT INTO grocery_lists (id, name, "ownerId", "createdAt")
           VALUES ('list-1', 'Shop', 'owner-1', 0)"#
    )
    .execute(&pool)
    .await
    .unwrap();

    for (code, expires_at) in [
        ("LIVEONE1", Utc::now() + Duration::hours(1)),
        // Expired, but inside the retention window: still answerable in support.
        ("RECENT01", Utc::now() - Duration::hours(1)),
        (
            "DEADONE1",
            Utc::now() - Duration::hours(INVITE_RETENTION_HOURS + 1),
        ),
    ] {
        sqlx::query!(
            r#"INSERT INTO list_invites (code, "listId", "createdBy", "expiresAt")
               VALUES ($1, 'list-1', 'owner-1', $2)"#,
            code,
            expires_at
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
            "INSERT INTO list_join_failures (user_id, failed_at) VALUES ('guesser', $1)",
            failed_at
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    let summary = reap_list_invites(&pool).await.unwrap();
    assert_eq!(
        summary,
        ListInviteReapSummary {
            invites_deleted: 1,
            join_failures_deleted: 1,
        }
    );

    let remaining: Vec<String> = sqlx::query_scalar!("SELECT code FROM list_invites ORDER BY code")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        remaining,
        vec!["LIVEONE1".to_string(), "RECENT01".to_string()]
    );
}
