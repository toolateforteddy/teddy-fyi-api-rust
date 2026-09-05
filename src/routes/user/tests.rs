use axum::extract::State;
use axum::Extension;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::tokens::Claims;
use crate::routes::sync::parse_or_hash_uuid;
use crate::routes::sync::tests::helpers::{seed_device, setup_state};
use crate::routes::user::handlers::delete_user_data_handler;

fn claims(user_id: &str) -> Claims {
    Claims {
        sub: user_id.to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10000000000,
        product: None,
    }
}

/// Gives `user_id` one of everything: an owned grocery list with an item, a store, a
/// category, a todo list with an item, a device with a config and a drawing, and a session.
async fn seed_user(pool: &PgPool, user_id: &str, suffix: &str) -> Uuid {
    let list_id = format!("list-{}", suffix);
    sqlx::query!(
        r#"INSERT INTO grocery_lists (id, name, "ownerId", "createdAt", version, is_deleted, sync_state)
           VALUES ($1, $2, $3, 0, 1, FALSE, 'SYNCED')"#,
        list_id,
        "Home List",
        user_id
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        r#"INSERT INTO grocery_list_members (id, "listId", "userId", role, "joinedAt", version, is_deleted, sync_state)
           VALUES ($1, $2, $3, 'OWNER', 0, 1, FALSE, 'SYNCED')"#,
        format!("member-owner-{}", suffix),
        list_id,
        user_id
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        r#"INSERT INTO stores (id, name, position, "isDefaultSupported", "userId", "listId", version, is_deleted, sync_state)
           VALUES ($1, $2, 0, TRUE, $3, $4, 1, FALSE, 'SYNCED')"#,
        format!("store-{}", suffix),
        "Corner Shop",
        user_id,
        list_id
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        r#"INSERT INTO categories (id, name, position, "userId", "listId", version, is_deleted, sync_state)
           VALUES ($1, $2, 0, $3, $4, 1, FALSE, 'SYNCED')"#,
        format!("category-{}", suffix),
        "Produce",
        user_id,
        list_id
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        r#"INSERT INTO grocery_items (id, name, quantity, "isBought", "createdAt", position, "timesBought", "userId", "isActive", "listId", version, is_deleted)
           VALUES ($1, $2, '1', FALSE, 0, 0, 0, $3, TRUE, $4, 1, FALSE)"#,
        format!("item-{}", suffix),
        "Apples",
        user_id,
        list_id
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        r#"INSERT INTO grocery_item_store_info ("groceryItemId", "storeId", price, "isAvailable", "userId", version, is_deleted, sync_state)
           VALUES ($1, $2, 1.5, TRUE, $3, 1, FALSE, 'SYNCED')"#,
        format!("item-{}", suffix),
        format!("store-{}", suffix),
        user_id
    )
    .execute(pool)
    .await
    .unwrap();

    let todo_list_id = format!("todo-list-{}", suffix);
    sqlx::query!(
        r#"INSERT INTO todo_lists (id, name, "colorHex", "userId", "createdAt", sync_state, version, is_deleted)
           VALUES ($1, $2, '#FFFFFF', $3, 0, 'SYNCED', 1, FALSE)"#,
        todo_list_id,
        "Chores",
        user_id
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        r#"INSERT INTO todo_items (id, title, "isCompleted", "createdAt", position, "scheduledAt", "userId", "isDaily", "listId", priority, sync_state, version, is_deleted)
           VALUES ($1, $2, FALSE, 0, 0, 0, $3, FALSE, $4, 0, 'SYNCED', 1, FALSE)"#,
        format!("todo-item-{}", suffix),
        "Sweep",
        user_id,
        todo_list_id
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        r#"INSERT INTO list_invites (code, "listId", "createdBy", "expiresAt") VALUES ($1, $2, $3, $4)"#,
        format!("CODE{}", suffix.to_uppercase()),
        list_id,
        user_id,
        Utc::now() + chrono::Duration::hours(24)
    )
    .execute(pool)
    .await
    .unwrap();

    let user_uuid = parse_or_hash_uuid(user_id);
    let device_uuid = seed_device(pool, user_uuid, "BouncyMeadowAdventure").await;

    sqlx::query!(
        r#"INSERT INTO configs (id, user_id, client_uuid, device_uuid, version, is_deleted, last_modified, key, value)
           VALUES ($1, $2, $3, $4, 1, FALSE, 0, 'theme', 'dark')"#,
        Uuid::new_v4(),
        user_uuid,
        Uuid::new_v4(),
        device_uuid
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        r#"INSERT INTO drawings (id, user_id, client_uuid, device_uuid, version, is_deleted, last_modified, created_at, data)
           VALUES ($1, $2, $3, $4, 1, FALSE, 0, 0, '{}'::jsonb)"#,
        Uuid::new_v4(),
        user_uuid,
        Uuid::new_v4(),
        device_uuid
    )
    .execute(pool)
    .await
    .unwrap();

    // A claimed pairing row and a failed claim, both keyed by the auth subject: the two
    // tables the erase used to miss.
    sqlx::query!(
        "INSERT INTO device_authorizations
             (device_code_hash, user_code, client_uuid, user_id, expires_at, claimed_at)
         VALUES ($1, $2, 'client-1', $3, $4, now())",
        format!("hash-{}", suffix),
        format!("USERCD{}", suffix.to_uppercase()),
        user_id,
        Utc::now() + chrono::Duration::minutes(10)
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO device_claim_failures (user_id) VALUES ($1)",
        user_id
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO users (id, email) VALUES ($1, $2)",
        user_id,
        format!("{}@teddy.fyi", user_id)
    )
    .execute(pool)
    .await
    .unwrap();

    sqlx::query!(
        "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at) VALUES ($1, 'client-1', 'hash', $2)",
        user_id,
        Utc::now() + chrono::Duration::days(7)
    )
    .execute(pool)
    .await
    .unwrap();

    device_uuid
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar::<_, i64>(sql).fetch_one(pool).await.unwrap()
}

#[sqlx::test]
async fn test_delete_user_data_removes_everything_the_user_owns(pool: PgPool) {
    seed_user(&pool, "user-1", "a").await;

    let response = delete_user_data_handler(
        State(setup_state(pool.clone())),
        Extension(claims("user-1")),
    )
    .await
    .expect("Deletion should succeed")
    .0;

    assert_eq!(response.user_id, "user-1");
    assert_eq!(response.deleted.grocery_lists, 1);
    assert_eq!(response.deleted.grocery_items, 1);
    assert_eq!(response.deleted.grocery_item_store_info, 1);
    assert_eq!(response.deleted.stores, 1);
    assert_eq!(response.deleted.categories, 1);
    assert_eq!(response.deleted.grocery_list_members, 1);
    assert_eq!(response.deleted.list_invites, 1);
    assert_eq!(response.deleted.todo_lists, 1);
    assert_eq!(response.deleted.todo_items, 1);
    assert_eq!(response.deleted.configs, 1);
    assert_eq!(response.deleted.drawings, 1);
    assert_eq!(response.deleted.devices, 1);
    assert_eq!(response.deleted.device_authorizations, 1);
    assert_eq!(response.deleted.device_claim_failures, 1);
    assert_eq!(response.deleted.sessions, 1);
    assert_eq!(response.deleted.users, 1);

    for table in [
        "grocery_lists",
        "grocery_list_members",
        "grocery_items",
        "grocery_item_store_info",
        "stores",
        "categories",
        "list_invites",
        "todo_lists",
        "todo_items",
        "configs",
        "drawings",
        "devices",
        "device_authorizations",
        "device_claim_failures",
        "sessions",
        "users",
    ] {
        assert_eq!(
            count(&pool, &format!("SELECT COUNT(*) FROM {}", table)).await,
            0,
            "{} should be empty after deletion",
            table
        );
    }
}

#[sqlx::test]
async fn test_delete_user_data_leaves_another_users_data_alone(pool: PgPool) {
    seed_user(&pool, "user-1", "a").await;
    seed_user(&pool, "user-2", "b").await;

    let _ = delete_user_data_handler(
        State(setup_state(pool.clone())),
        Extension(claims("user-1")),
    )
    .await
    .expect("Deletion should succeed");

    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM grocery_items WHERE \"userId\" = 'user-2'").await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM todo_items WHERE \"userId\" = 'user-2'").await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM users WHERE id = 'user-2'").await,
        1
    );
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM devices").await, 1);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM configs").await, 1);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM drawings").await, 1);
    assert_eq!(
        count(
            &pool,
            "SELECT COUNT(*) FROM device_authorizations WHERE user_id = 'user-2'"
        )
        .await,
        1
    );
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM device_authorizations").await, 1);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM device_claim_failures").await, 1);
}

/// A pairing code nobody has claimed yet has a NULL `user_id` and belongs to no account,
/// so an erase must leave it for the pairing reaper rather than sweep it up.
#[sqlx::test]
async fn test_delete_user_data_leaves_unclaimed_pairing_codes_alone(pool: PgPool) {
    seed_user(&pool, "user-1", "a").await;

    sqlx::query!(
        "INSERT INTO device_authorizations
             (device_code_hash, user_code, client_uuid, expires_at)
         VALUES ('hash-open', 'OPENCODE', 'client-9', $1)",
        Utc::now() + chrono::Duration::minutes(10)
    )
    .execute(&pool)
    .await
    .unwrap();

    let response = delete_user_data_handler(
        State(setup_state(pool.clone())),
        Extension(claims("user-1")),
    )
    .await
    .expect("Deletion should succeed")
    .0;

    assert_eq!(response.deleted.device_authorizations, 1);
    let remaining: Vec<String> =
        sqlx::query_scalar!("SELECT user_code FROM device_authorizations")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(remaining, vec!["OPENCODE".to_string()]);
}

/// The erase is one transaction, so a failure at any point leaves the account exactly as
/// it was. This is also what lets the reaper dry-run by rolling back.
#[sqlx::test]
async fn test_delete_user_data_is_atomic(pool: PgPool) {
    seed_user(&pool, "user-1", "a").await;

    let mut tx = pool.begin().await.unwrap();
    let (deleted, _) = crate::routes::user::deletion::delete_user_data(&mut tx, "user-1")
        .await
        .unwrap();
    assert_eq!(deleted.device_authorizations, 1);
    assert_eq!(deleted.device_claim_failures, 1);
    tx.rollback().await.unwrap();

    for table in [
        "users",
        "sessions",
        "devices",
        "device_authorizations",
        "device_claim_failures",
        "grocery_lists",
        "drawings",
    ] {
        assert_eq!(
            count(&pool, &format!("SELECT COUNT(*) FROM {}", table)).await,
            1,
            "{} must survive a rolled-back erase",
            table
        );
    }
}

/// A list the caller only joined survives; they just stop being a member of it. The
/// owner's own rows on that list are untouched.
#[sqlx::test]
async fn test_delete_user_data_only_drops_membership_of_joined_lists(pool: PgPool) {
    seed_user(&pool, "user-2", "b").await;

    sqlx::query!(
        r#"INSERT INTO grocery_list_members (id, "listId", "userId", role, "joinedAt", version, is_deleted, sync_state)
           VALUES ('member-guest', 'list-b', 'user-1', 'MEMBER', 0, 1, FALSE, 'SYNCED')"#
    )
    .execute(&pool)
    .await
    .unwrap();

    let response = delete_user_data_handler(
        State(setup_state(pool.clone())),
        Extension(claims("user-1")),
    )
    .await
    .expect("Deletion should succeed")
    .0;

    assert_eq!(response.deleted.grocery_list_members, 1);
    assert_eq!(response.deleted.grocery_lists, 0);
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM grocery_lists WHERE id = 'list-b'").await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM grocery_list_members WHERE \"userId\" = 'user-1'").await,
        0
    );
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM grocery_list_members WHERE \"userId\" = 'user-2'").await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM grocery_items WHERE \"listId\" = 'list-b'").await,
        1
    );
}

/// Deleting an account with nothing in it is a no-op that still succeeds, so a client can
/// retry the call safely.
#[sqlx::test]
async fn test_delete_user_data_is_idempotent(pool: PgPool) {
    seed_user(&pool, "user-1", "a").await;

    let _ = delete_user_data_handler(
        State(setup_state(pool.clone())),
        Extension(claims("user-1")),
    )
    .await
    .expect("First deletion should succeed");

    let second = delete_user_data_handler(
        State(setup_state(pool.clone())),
        Extension(claims("user-1")),
    )
    .await
    .expect("Second deletion should succeed")
    .0;

    assert_eq!(second.deleted, Default::default());
}
