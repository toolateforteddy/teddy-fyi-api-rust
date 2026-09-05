use chrono::{Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::jobs::reap_stale_users::{
    find_stale_users, parse_dry_run, parse_inactive_months, reap_stale_users, ReapConfig,
};
use crate::routes::sync::parse_or_hash_uuid;
use crate::routes::sync::tests::helpers::setup_state;

/// Inserts a user plus one device whose most recent sync is `months_ago`, mirroring a
/// tablet that checked in and then went quiet.
async fn seed_synced_user(pool: &PgPool, user_id: &str, months_ago: i64) -> Uuid {
    seed_user_row(pool, user_id).await;

    let user_uuid = parse_or_hash_uuid(user_id);
    let device_uuid = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO devices (id, user_id, name, last_seen_at) VALUES ($1, $2, $3, $4)",
        device_uuid,
        user_uuid,
        "BouncyMeadowAdventure",
        Utc::now() - Duration::days(months_ago * 30)
    )
    .execute(pool)
    .await
    .unwrap();

    device_uuid
}

async fn seed_user_row(pool: &PgPool, user_id: &str) {
    sqlx::query!(
        "INSERT INTO users (id, email) VALUES ($1, $2)",
        user_id,
        format!("{}@example.com", user_id)
    )
    .execute(pool)
    .await
    .unwrap();
}

fn armed() -> ReapConfig {
    ReapConfig { inactive_months: 12, dry_run: false }
}

async fn user_exists(pool: &PgPool, user_id: &str) -> bool {
    sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM users WHERE id = $1) AS "exists!""#,
        user_id
    )
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn test_finds_only_users_past_the_cutoff(pool: PgPool) {
    seed_synced_user(&pool, "stale-user", 14).await;
    seed_synced_user(&pool, "recent-user", 2).await;

    let cutoff = Utc::now() - Duration::days(365);
    let (stale, scanned) = find_stale_users(&pool, cutoff).await.unwrap();

    assert_eq!(scanned, 2, "both accounts have devices, so both are in scope");
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0].user_id, "stale-user");
}

/// The policy restarts the clock from the most recent sync, so one live device keeps an
/// account even when its other devices went quiet years ago.
#[sqlx::test]
async fn test_most_recent_device_sync_wins(pool: PgPool) {
    seed_synced_user(&pool, "user-1", 30).await;

    let user_uuid = parse_or_hash_uuid("user-1");
    sqlx::query!(
        "INSERT INTO devices (id, user_id, name, last_seen_at) VALUES ($1, $2, $3, $4)",
        Uuid::new_v4(),
        user_uuid,
        "SecondTablet",
        Utc::now() - Duration::days(3)
    )
    .execute(&pool)
    .await
    .unwrap();

    let cutoff = Utc::now() - Duration::days(365);
    let (stale, _) = find_stale_users(&pool, cutoff).await.unwrap();

    assert!(stale.is_empty(), "the newer device's sync should reset the clock");
}

/// A grocery/todo account never gets a `devices` row, and no published policy sets a
/// retention window for it. It has to stay out of the sweep entirely rather than read as
/// infinitely stale.
#[sqlx::test]
async fn test_users_without_devices_are_out_of_scope(pool: PgPool) {
    seed_user_row(&pool, "grocery-only-user").await;

    let cutoff = Utc::now() - Duration::days(365);
    let (stale, scanned) = find_stale_users(&pool, cutoff).await.unwrap();

    assert_eq!(scanned, 0);
    assert!(stale.is_empty());

    let summary = reap_stale_users(&pool, &setup_state(pool.clone()).redis_publisher, &armed())
        .await
        .unwrap();
    assert_eq!(summary.deleted, 0);
    assert!(user_exists(&pool, "grocery-only-user").await);
}

/// Every device the `20260901120000` backfill created has a NULL `last_seen_at`. Reading
/// those as "never synced" would delete the whole install on the first run.
#[sqlx::test]
async fn test_never_synced_device_falls_back_to_its_creation(pool: PgPool) {
    seed_user_row(&pool, "backfilled-user").await;
    sqlx::query!(
        "INSERT INTO devices (id, user_id, name) VALUES ($1, $2, $3)",
        Uuid::new_v4(),
        parse_or_hash_uuid("backfilled-user"),
        "BackfilledTablet"
    )
    .execute(&pool)
    .await
    .unwrap();

    let cutoff = Utc::now() - Duration::days(365);
    let (stale, scanned) = find_stale_users(&pool, cutoff).await.unwrap();

    assert_eq!(scanned, 1, "the account is in scope");
    assert!(stale.is_empty(), "a just-backfilled device starts a fresh 12-month clock");
}

#[sqlx::test]
async fn test_dry_run_reports_without_deleting(pool: PgPool) {
    seed_synced_user(&pool, "stale-user", 14).await;

    let config = ReapConfig { inactive_months: 12, dry_run: true };
    let summary = reap_stale_users(&pool, &setup_state(pool.clone()).redis_publisher, &config)
        .await
        .unwrap();

    assert_eq!(summary.eligible, 1);
    assert_eq!(summary.deleted, 0);
    assert_eq!(summary.failed, 0);
    assert!(summary.dry_run);
    assert!(user_exists(&pool, "stale-user").await, "dry run must roll its erase back");
}

#[sqlx::test]
async fn test_armed_run_deletes_stale_and_spares_recent(pool: PgPool) {
    seed_synced_user(&pool, "stale-user", 14).await;
    seed_synced_user(&pool, "recent-user", 2).await;

    let summary = reap_stale_users(&pool, &setup_state(pool.clone()).redis_publisher, &armed())
        .await
        .unwrap();

    assert_eq!(summary.eligible, 1);
    assert_eq!(summary.deleted, 1);
    assert_eq!(summary.failed, 0);

    assert!(!user_exists(&pool, "stale-user").await);
    assert!(user_exists(&pool, "recent-user").await);

    let devices = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM devices WHERE user_id = $1"#,
        parse_or_hash_uuid("stale-user")
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(devices, 0, "the account's devices go with it");
}

/// Arming the job takes an explicit `REAP_DRY_RUN=false`; anything else stays safe.
#[test]
fn test_only_an_explicit_false_arms_the_job() {
    assert!(parse_dry_run(None));
    assert!(parse_dry_run(Some("true".to_string())));
    assert!(parse_dry_run(Some(String::new())));
    assert!(parse_dry_run(Some("no".to_string())), "a typo must not arm the job");

    assert!(!parse_dry_run(Some("false".to_string())));
    assert!(!parse_dry_run(Some(" FALSE ".to_string())));
}

#[test]
fn test_inactivity_window_falls_back_to_the_published_twelve_months() {
    assert_eq!(parse_inactive_months(None), 12);
    assert_eq!(parse_inactive_months(Some("banana".to_string())), 12);
    assert_eq!(parse_inactive_months(Some("0".to_string())), 12);
    assert_eq!(parse_inactive_months(Some(" 6 ".to_string())), 6);
}
