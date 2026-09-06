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
