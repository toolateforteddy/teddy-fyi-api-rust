use crate::routes::sync::{
    find_affected_grocery_users, GroceryChangeDelta, GroceryListChangeDelta,
    GroceryListMemberChangeDelta, OperationType, SharedRequest, SyncRequest,
};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use crate::routes::sync::tests::helpers::request;

/// A request with nothing in it. Each test fills in only the change vector it cares about,
/// which keeps the interesting part of every test to a couple of lines.
fn empty_request() -> SyncRequest {
    request("client-1")
}

/// Seeds a list with an owner and, optionally, extra members. Every row is stamped with the
/// same `updated_at` the caller passes so a test can deliberately collide two users' writes
/// on one timestamp — the situation the old `WHERE updated_at = $1` query could not tell
/// apart.
async fn seed_list(
    pool: &PgPool,
    list_id: &str,
    owner: &str,
    extra_members: &[&str],
    updated_at: DateTime<Utc>,
) {
    sqlx::query!(
        r#"INSERT INTO grocery_lists (id, name, "ownerId", "createdAt", version, updated_at, updated_by_client, is_deleted, sync_state)
           VALUES ($1, $2, $3, 0, 1, $4, 'seed', false, 'SYNCED')"#,
        list_id,
        list_id,
        owner,
        updated_at,
    )
    .execute(pool)
    .await
    .unwrap();

    for (idx, member) in std::iter::once(&owner)
        .chain(extra_members.iter())
        .enumerate()
    {
        sqlx::query!(
            r#"INSERT INTO grocery_list_members (id, "listId", "userId", role, "joinedAt", version, is_deleted, sync_state, updated_at, updated_by_client)
               VALUES ($1, $2, $3, $4, 0, 1, false, 'SYNCED', $5, 'seed')"#,
            format!("{}-member-{}", list_id, idx),
            list_id,
            member,
            if idx == 0 { "OWNER" } else { "MEMBER" },
            updated_at,
        )
        .execute(pool)
        .await
        .unwrap();
    }
}

async fn seed_item(pool: &PgPool, item_id: &str, list_id: &str, updated_at: DateTime<Utc>) {
    sqlx::query!(
        r#"INSERT INTO grocery_items (id, name, quantity, "isBought", "createdAt", position, "timesBought", "userId", "isActive", "listId", version, is_deleted, sync_state, updated_at, updated_by_client)
           VALUES ($1, $2, '1', false, 0, 0, 0, NULL, true, $3, 1, false, 'SYNCED', $4, 'seed')"#,
        item_id,
        item_id,
        list_id,
        updated_at,
    )
    .execute(pool)
    .await
    .unwrap();
}

/// The whole reason the query is wider than "the caller": a write to a shared list has to
/// reach every co-member, or their client is never told there is anything to pull.
#[sqlx::test]
async fn test_affected_users_includes_every_co_member_of_a_touched_list(pool: PgPool) {
    let now = Utc::now();
    seed_list(&pool, "shared-list", "user-a", &["user-b", "user-c"], now).await;
    seed_item(&pool, "shared-item", "shared-list", now).await;

    let mut payload = empty_request();
    payload.grocery_changes = vec![GroceryChangeDelta {
        id: "shared-item".to_string(),
        operation_type: OperationType::Update,
        version: 1,
        data: None,
    }];

    let mut tx = pool.begin().await.unwrap();
    let mut users = find_affected_grocery_users(&mut tx, "user-a", &payload)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    users.sort();

    assert_eq!(users, vec!["user-a", "user-b", "user-c"]);
}

/// Naming the list row itself, or a membership row on it, has to reach the same set — the
/// three routes into a list must not disagree.
#[sqlx::test]
async fn test_affected_users_resolves_lists_named_directly_or_by_membership(pool: PgPool) {
    let now = Utc::now();
    seed_list(&pool, "shared-list", "user-a", &["user-b"], now).await;

    let mut by_list = empty_request();
    by_list.grocery_list_changes = vec![GroceryListChangeDelta {
        id: "shared-list".to_string(),
        operation_type: OperationType::Update,
        version: 1,
        data: None,
    }];

    let mut by_member = empty_request();
    by_member.grocery_list_member_changes = vec![GroceryListMemberChangeDelta {
        id: "shared-list-member-1".to_string(),
        operation_type: OperationType::Update,
        version: 1,
        data: None,
    }];

    for payload in [by_list, by_member] {
        let mut tx = pool.begin().await.unwrap();
        let mut users = find_affected_grocery_users(&mut tx, "user-a", &payload)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        users.sort();
        assert_eq!(users, vec!["user-a", "user-b"]);
    }
}

/// The regression this change exists for. Two unrelated users write at the very same
/// `updated_at`; the old query matched on that timestamp alone and so pulled the other
/// user's list into this caller's affected set. Reaching the lists through the ids the
/// request named, filtered by what the caller is party to, makes the collision irrelevant.
#[sqlx::test]
async fn test_affected_users_excludes_an_unrelated_concurrent_writer(pool: PgPool) {
    // One instant, shared by both users' rows.
    let collision = Utc::now();
    seed_list(&pool, "list-a", "user-a", &[], collision).await;
    seed_item(&pool, "item-a", "list-a", collision).await;

    seed_list(&pool, "list-x", "user-x", &["user-y"], collision).await;
    seed_item(&pool, "item-x", "list-x", collision).await;

    let mut payload = empty_request();
    payload.grocery_changes = vec![GroceryChangeDelta {
        id: "item-a".to_string(),
        operation_type: OperationType::Update,
        version: 1,
        data: None,
    }];

    let mut tx = pool.begin().await.unwrap();
    let users = find_affected_grocery_users(&mut tx, "user-a", &payload)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert_eq!(users, vec!["user-a"]);
    assert!(!users.contains(&"user-x".to_string()));
    assert!(!users.contains(&"user-y".to_string()));
}

/// Ids are client-supplied, so a caller can name a list they have nothing to do with. That
/// must not let them steer whose caches get touched.
#[sqlx::test]
async fn test_affected_users_ignores_a_list_the_caller_cannot_reach(pool: PgPool) {
    let now = Utc::now();
    seed_list(&pool, "list-x", "user-x", &["user-y"], now).await;
    seed_item(&pool, "item-x", "list-x", now).await;

    let mut payload = empty_request();
    payload.grocery_changes = vec![GroceryChangeDelta {
        id: "item-x".to_string(),
        operation_type: OperationType::Update,
        version: 1,
        data: None,
    }];

    let mut tx = pool.begin().await.unwrap();
    let users = find_affected_grocery_users(&mut tx, "user-a", &payload)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    assert!(users.is_empty(), "unreachable list leaked users: {:?}", users);
}

/// A member who was just removed still has to be told, or their client keeps showing a list
/// it no longer belongs to.
#[sqlx::test]
async fn test_affected_users_still_reaches_a_removed_member(pool: PgPool) {
    let now = Utc::now();
    seed_list(&pool, "shared-list", "user-a", &["user-b"], now).await;
    sqlx::query!(
        r#"UPDATE grocery_list_members SET is_deleted = true WHERE "userId" = 'user-b'"#
    )
    .execute(&pool)
    .await
    .unwrap();

    let mut payload = empty_request();
    payload.grocery_list_member_changes = vec![GroceryListMemberChangeDelta {
        id: "shared-list-member-1".to_string(),
        operation_type: OperationType::Delete,
        version: 1,
        data: None,
    }];

    let mut tx = pool.begin().await.unwrap();
    let mut users = find_affected_grocery_users(&mut tx, "user-a", &payload)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    users.sort();

    assert_eq!(users, vec!["user-a", "user-b"]);
}

/// Structural check for the other half of this change: the three sync futures are handed
/// handles on one request body, not three deep copies of it. Asserted by identity — the
/// handles point at the same allocation, and so do the vectors inside it — rather than by
/// measuring, which would only ever be a proxy.
#[test]
fn test_shared_request_hands_out_one_allocation() {
    let shared = SharedRequest::new(empty_request());

    let todo = shared.handle();
    let grocery = shared.handle();
    let config = shared.handle();

    assert!(std::sync::Arc::ptr_eq(&todo, &grocery));
    assert!(std::sync::Arc::ptr_eq(&grocery, &config));
    // Three handles plus the `SharedRequest` itself.
    assert_eq!(std::sync::Arc::strong_count(&todo), 4);

    // The payload's own heap buffers are shared too, which is the part that matters: a
    // `SyncRequest::clone` would have reallocated every one of these.
    assert_eq!(todo.drawings.as_ptr(), config.drawings.as_ptr());
    assert_eq!(todo.grocery_changes.as_ptr(), config.grocery_changes.as_ptr());
    assert_eq!(todo.client_id.as_ptr(), config.client_id.as_ptr());
}
