//! What one download hands back: how much of it, and how many times each row appears.
//!
//! Two behaviours are pinned here. The page bound (`crate::routes::sync::paging`), which
//! is what stops an initial sync from serialising an account's entire drawing table into
//! one response; and the fact that a single row now reaches each wire channel exactly
//! once, which is what the shared read is for.

use axum::extract::State;
use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use crate::routes::sync::tests::helpers::{request, seed_device, setup_state, sync_handler};
use crate::routes::sync::{
    AppJson, SyncRequest, SyncScope, fetch_drawing_download, parse_or_hash_uuid,
};

/// Inserts `count` drawings owned by `user_uuid`, each on its own millisecond so that a
/// page has somewhere to stop.
async fn seed_drawings(
    pool: &PgPool,
    user_uuid: Uuid,
    device_uuid: Uuid,
    client_uuid: Uuid,
    base_ms: i64,
    count: i64,
) -> Vec<Uuid> {
    let mut ids = Vec::new();
    for n in 0..count {
        let id = Uuid::new_v4();
        sqlx::query!(
            "INSERT INTO drawings (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data) \
             VALUES ($1, $2, $3, $4, 1, FALSE, $5, 'SYNCED'::sync_state, 1000, $6)",
            id,
            user_uuid,
            device_uuid,
            client_uuid,
            base_ms + n,
            serde_json::json!({ "strokes": [n] })
        )
        .execute(pool)
        .await
        .unwrap();
        ids.push(id);
    }
    ids
}

/// Inserts `count` drawings that all share one `last_modified`, the shape one oversized
/// upload leaves behind.
async fn seed_drawings_same_millisecond(
    pool: &PgPool,
    user_uuid: Uuid,
    device_uuid: Uuid,
    client_uuid: Uuid,
    ms: i64,
    count: i64,
) {
    for n in 0..count {
        sqlx::query!(
            "INSERT INTO drawings (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data) \
             VALUES ($1, $2, $3, $4, 1, FALSE, $5, 'SYNCED'::sync_state, 1000, $6)",
            Uuid::new_v4(),
            user_uuid,
            device_uuid,
            client_uuid,
            ms,
            serde_json::json!({ "strokes": [n] })
        )
        .execute(pool)
        .await
        .unwrap();
    }
}

#[sqlx::test]
async fn a_page_exactly_at_the_limit_is_not_truncated(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;
    let other_client = parse_or_hash_uuid("client-2");
    seed_drawings(&pool, user_uuid, device_uuid, other_client, 1_000, 4).await;

    let mut tx = pool.begin().await.unwrap();
    let page = fetch_drawing_download(
        &mut tx,
        &user_uuid,
        &parse_or_hash_uuid("client-1"),
        None,
        None,
        Some(4),
    )
    .await
    .unwrap();

    assert_eq!(page.items.len(), 4);
    assert_eq!(page.remote_changes.len(), 4);
    // Nothing was held back, so the client's cursor may advance to the request's own
    // timestamp exactly as it did before pagination existed.
    assert_eq!(page.next_cursor_ms, None);
}

#[sqlx::test]
async fn one_row_over_the_limit_pages(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;
    let other_client = parse_or_hash_uuid("client-2");
    seed_drawings(&pool, user_uuid, device_uuid, other_client, 1_000, 5).await;

    let mut tx = pool.begin().await.unwrap();
    let first = fetch_drawing_download(
        &mut tx,
        &user_uuid,
        &parse_or_hash_uuid("client-1"),
        None,
        None,
        Some(4),
    )
    .await
    .unwrap();

    assert_eq!(first.items.len(), 4);
    assert_eq!(first.next_cursor_ms, Some(1_003));

    // Resuming from the reported cursor returns the remainder and nothing twice, which is
    // the whole point of the cursor landing on a whole millisecond.
    let resumed = fetch_drawing_download(
        &mut tx,
        &user_uuid,
        &parse_or_hash_uuid("client-1"),
        None,
        Some(chrono::DateTime::from_timestamp_millis(1_003).unwrap()),
        Some(4),
    )
    .await
    .unwrap();

    assert_eq!(resumed.items.len(), 1);
    assert_eq!(resumed.next_cursor_ms, None);

    let mut seen: Vec<Uuid> = first.items.iter().chain(resumed.items.iter()).map(|d| d.id).collect();
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 5, "every drawing arrived exactly once across the two pages");
}

#[sqlx::test]
async fn a_page_that_would_split_a_millisecond_stops_before_it(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;
    let other_client = parse_or_hash_uuid("client-2");
    seed_drawings(&pool, user_uuid, device_uuid, other_client, 1_000, 2).await;
    seed_drawings_same_millisecond(&pool, user_uuid, device_uuid, other_client, 1_002, 3).await;

    let mut tx = pool.begin().await.unwrap();
    let page = fetch_drawing_download(
        &mut tx,
        &user_uuid,
        &parse_or_hash_uuid("client-1"),
        None,
        None,
        Some(3),
    )
    .await
    .unwrap();

    // The page edge falls inside the group at 1_002, and a `>` cursor cannot address a
    // position inside one millisecond, so the group waits for the next round entire.
    assert_eq!(page.items.len(), 2);
    assert_eq!(page.next_cursor_ms, Some(1_001));
}

#[sqlx::test]
async fn more_than_a_page_in_one_millisecond_is_served_whole(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;
    let other_client = parse_or_hash_uuid("client-2");
    seed_drawings_same_millisecond(&pool, user_uuid, device_uuid, other_client, 1_000, 5).await;

    let mut tx = pool.begin().await.unwrap();
    let page = fetch_drawing_download(
        &mut tx,
        &user_uuid,
        &parse_or_hash_uuid("client-1"),
        None,
        None,
        Some(2),
    )
    .await
    .unwrap();

    // There is no boundary below the group to stop at, so serving it whole is the only
    // way the client ever gets past it — over the page size, deliberately.
    assert_eq!(page.items.len(), 5);
    assert_eq!(page.next_cursor_ms, Some(1_000));
}

#[sqlx::test]
async fn a_cloud_sync_carries_each_drawing_once_per_channel(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;
    let other_client = parse_or_hash_uuid("client-2");
    let ids = seed_drawings(&pool, user_uuid, device_uuid, other_client, Utc::now().timestamp_millis(), 3).await;

    let req = SyncRequest {
        last_synced_at: Some(Utc::now() - chrono::Duration::minutes(5)),
        scope: Some(SyncScope::ScribbleKeepCloud),
        supports_paging: true,
        ..request("client-1")
    };

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    for id in &ids {
        assert_eq!(
            res.drawings.iter().filter(|d| d.id == *id).count(),
            1,
            "drawing {} should appear once in `drawings`",
            id
        );
        assert_eq!(
            res.remote_drawing_changes.iter().filter(|c| c.id == id.to_string()).count(),
            1,
            "drawing {} should appear once in `remote_drawing_changes`",
            id
        );
    }
    assert_eq!(res.drawings.len(), 3);
    assert_eq!(res.remote_drawing_changes.len(), 3);
    assert!(!res.has_more);
}

#[sqlx::test]
async fn a_client_that_cannot_page_is_served_whole(pool: PgPool) {
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;
    let other_client = parse_or_hash_uuid("client-2");
    seed_drawings(&pool, user_uuid, device_uuid, other_client, 1_000, 9).await;

    let mut tx = pool.begin().await.unwrap();
    let page = fetch_drawing_download(
        &mut tx,
        &user_uuid,
        &parse_or_hash_uuid("client-1"),
        None,
        None,
        None,
    )
    .await
    .unwrap();

    // Nine rows and no bound: all nine, and no cursor walked back, because there is
    // nothing left behind for the client to come back for.
    assert_eq!(page.items.len(), 9);
    assert_eq!(page.remote_changes.len(), 9);
    assert_eq!(page.next_cursor_ms, None);
}

#[sqlx::test]
async fn a_sync_that_does_not_declare_paging_is_not_truncated(pool: PgPool) {
    // The regression this guards. The shipped clients send no `supports_paging` and no
    // `last_synced_at`, so every one of their syncs is an initial sync. Bounding those at
    // a page they can never ask past would not slow their download down -- it would cost
    // them every row after the page, on every sync, permanently.
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;
    let other_client = parse_or_hash_uuid("client-2");
    let count = crate::routes::sync::limits::DEFAULT_SYNC_DOWNLOAD_PAGE_SIZE as i64 + 5;
    seed_drawings(&pool, user_uuid, device_uuid, other_client, 1_000, count).await;

    let req = SyncRequest {
        scope: Some(SyncScope::ScribbleKeepCloud),
        ..request("client-1")
    };

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    assert_eq!(res.drawings.len(), count as usize);
    assert_eq!(res.remote_drawing_changes.len(), count as usize);
    // Nothing was held back, so the cursor is this request's own clock reading and the
    // client is not being asked to come back for a page it cannot request.
    assert!(!res.has_more);
}

#[sqlx::test]
async fn a_sync_that_declares_paging_is_bounded_and_says_so(pool: PgPool) {
    let state = setup_state(pool.clone());
    let user_uuid = parse_or_hash_uuid("user-1");
    let device_uuid = seed_device(&pool, user_uuid, "Tablet").await;
    let other_client = parse_or_hash_uuid("client-2");
    let page_size = crate::routes::sync::limits::DEFAULT_SYNC_DOWNLOAD_PAGE_SIZE as i64;
    seed_drawings(&pool, user_uuid, device_uuid, other_client, 1_000, page_size + 5).await;

    let req = SyncRequest {
        scope: Some(SyncScope::ScribbleKeepCloud),
        supports_paging: true,
        ..request("client-1")
    };

    let res = sync_handler(State(state), AppJson(req))
        .await
        .expect("Handler should succeed")
        .0;

    assert_eq!(res.drawings.len(), page_size as usize);
    assert!(res.has_more, "a truncated download must say it was truncated");
}

// --- todo ---------------------------------------------------------------------------
//
// The todo tables page on the same rule as configs and drawings, against `updated_at`
// rather than a millisecond count (`paging::trim_page_at`). They are safe to page for a
// reason the grocery tables are not: inclusion here is `"userId" = $1 AND updated_at > $2`,
// so a row is in the download only when its own cursor column says so, and ordering by
// that column agrees with the filter. The grocery download's membership disjunct breaks
// exactly that property — see `grocery/remote_mutations.rs`.

use crate::routes::sync::fetch_remote_todo_mutations;

/// Inserts `count` todo items for `user_id`, each a second apart so a page can stop.
async fn seed_todos(pool: &PgPool, user_id: &str, count: i64) {
    for n in 0..count {
        sqlx::query!(
            "INSERT INTO todo_items (id, title, \"isCompleted\", \"createdAt\", position, \
             \"scheduledAt\", \"userId\", \"isDaily\", priority, sync_state, version, is_deleted, \
             updated_at, updated_by_client) \
             VALUES ($1, $2, FALSE, 0, 0, 0, $3, FALSE, 0, 'SYNCED', 1, FALSE, \
                     now() - interval '1 day' + ($4 || ' seconds')::interval, 'other-client')",
            format!("todo-{n}"),
            format!("Task {n}"),
            user_id,
            n.to_string()
        )
        .execute(pool)
        .await
        .unwrap();
    }
}

/// A todo download larger than one page stops at the page and says where to resume.
#[sqlx::test]
async fn a_large_todo_download_is_bounded_and_resumable(pool: PgPool) {
    seed_todos(&pool, "user-1", 25).await;

    let mut tx = pool.begin().await.unwrap();
    let first = fetch_remote_todo_mutations(&mut tx, "user-1", "client-1", None, Some(10))
        .await
        .unwrap();

    assert_eq!(first.changes.len(), 10, "the page bound was not applied");
    let cursor = first
        .next_cursor
        .expect("a truncated page must hand back a cursor");

    // Resuming from that cursor picks up where the page stopped, with no row delivered
    // twice and none skipped.
    let second = fetch_remote_todo_mutations(&mut tx, "user-1", "client-1", Some(cursor), Some(10))
        .await
        .unwrap();
    assert_eq!(second.changes.len(), 10);

    let first_ids: Vec<&str> = first.changes.iter().map(|c| c.id.as_str()).collect();
    let second_ids: Vec<&str> = second.changes.iter().map(|c| c.id.as_str()).collect();
    assert!(
        second_ids.iter().all(|id| !first_ids.contains(id)),
        "the second page repeated rows from the first: {first_ids:?} then {second_ids:?}"
    );

    // Walking the cursor to the end reaches every row exactly once.
    let mut seen = first.changes.len() + second.changes.len();
    let mut cursor = second.next_cursor.expect("still more to come");
    loop {
        let page = fetch_remote_todo_mutations(&mut tx, "user-1", "client-1", Some(cursor), Some(10))
            .await
            .unwrap();
        seen += page.changes.len();
        match page.next_cursor {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!(seen, 25, "walking the pages did not deliver every row exactly once");
}

/// A client that cannot resume is served whole, exactly as before paging existed.
#[sqlx::test]
async fn a_todo_download_for_a_client_without_paging_is_not_bounded(pool: PgPool) {
    seed_todos(&pool, "user-1", 25).await;

    let mut tx = pool.begin().await.unwrap();
    let all = fetch_remote_todo_mutations(&mut tx, "user-1", "client-1", None, None)
        .await
        .unwrap();

    assert_eq!(all.changes.len(), 25);
    assert!(all.next_cursor.is_none(), "an unpaged read must never truncate");
}

/// More rows than a page share one instant — the case a `>` cursor cannot split.
///
/// One sync request stamps every row it writes with the same clock reading, so this is
/// what a single large upload looks like on the next device's download. The instant is
/// served whole rather than half-delivered, because a cursor landing inside it would make
/// the remainder unreachable.
#[sqlx::test]
async fn a_todo_instant_bigger_than_a_page_is_served_whole(pool: PgPool) {
    // 15 rows sharing one updated_at, against a page size of 10.
    for n in 0..15 {
        sqlx::query!(
            "INSERT INTO todo_items (id, title, \"isCompleted\", \"createdAt\", position, \
             \"scheduledAt\", \"userId\", \"isDaily\", priority, sync_state, version, is_deleted, \
             updated_at, updated_by_client) \
             VALUES ($1, $2, FALSE, 0, 0, 0, 'user-1', FALSE, 0, 'SYNCED', 1, FALSE, \
                     TIMESTAMPTZ '2026-01-01 00:00:00Z', 'other-client')",
            format!("todo-{n}"),
            format!("Task {n}")
        )
        .execute(&pool)
        .await
        .unwrap();
    }

    let mut tx = pool.begin().await.unwrap();
    let page = fetch_remote_todo_mutations(&mut tx, "user-1", "client-1", None, Some(10))
        .await
        .unwrap();

    assert_eq!(
        page.changes.len(),
        15,
        "an instant larger than a page must be served whole, not split"
    );
    assert!(
        page.next_cursor.is_some(),
        "serving the instant whole still has to move the cursor onto it"
    );
}
