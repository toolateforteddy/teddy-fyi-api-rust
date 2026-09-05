use crate::routes::sync::types::*;
use sqlx::{Postgres, Transaction};

/// Works out which users need their grocery sync caches bumped because of the writes this
/// request just made.
///
/// # Why this exists at all
///
/// Grocery lists are collaborative. When one member adds an item, every *other* member's
/// `user:<id>:last_update:Grocery` cache key has to move forward, or their client keeps
/// asking `/api/sync/status`, being told "nothing new", and never pulling the item. So the
/// set we want is: everyone who can see a list that this request touched — which is
/// deliberately wider than "the caller".
///
/// # Why it is not a timestamp match
///
/// This used to be a `SELECT DISTINCT "userId" ... WHERE updated_at = $1` union over all six
/// grocery tables, bound only on the request's `server_timestamp`. That was wrong in two
/// ways that compound:
///
///   * It was not scoped to the caller at all. Every sync carrying grocery changes scanned
///     every user's grocery rows, so the cost grew with the size of the whole table rather
///     than with the caller's own data.
///   * `updated_at = $1` is an equality on a wall-clock timestamp, so two requests that
///     happen to commit at the same instant match each other's rows. An unrelated user's
///     lists could then land in this caller's affected-user set.
///
/// So the basis here is the ids the request actually names, not "whatever the clock says".
///
/// # Why the payload ids and not `success_ids`
///
/// The obvious alternative is to thread the written ids back out of the `process_*` calls.
/// The ids are in fact already collected — in `success_ids` — but that vector is a single
/// flat `Vec<String>` that mixes all six entity types with no way to tell them apart, and
/// for `grocery_item_store_info` it holds a synthesised `"<itemId>-<storeId>"` key that
/// matches no column in any table. It cannot address the rows. Reshaping all six
/// `process_*` signatures to return typed id sets would be a much larger change for the
/// same result, because the payload ids are already an *upper bound* on what was written:
/// a change that fails authorization aborts the whole transaction, and one that is silently
/// skipped only ever leaves an id in this set that the reachability filter below then has
/// to justify anyway. Over-listing an id is therefore inert; under-listing one would break
/// a co-member's sync, which is the failure that matters.
///
/// # The scoping rule
///
/// A touched list only counts if the caller can legitimately reach it — they own it, or
/// they have a membership row on it. That is exactly the authority the `process_*`
/// functions already enforce on the writes themselves, so it adds no new policy; it just
/// stops ids the caller made up from steering whose caches get touched. Membership is
/// accepted regardless of `is_deleted`, because a member removing *themselves* from a list
/// must still notify the members who remain.
///
/// The notification set is then every membership row on those lists plus each list's owner.
/// Deleted membership rows are included on purpose: a just-removed member needs to hear
/// about it so their client drops the list.
///
/// Rows with no `listId` — a user's private stores, categories and items — are not queried
/// at all any more. Scoped to the caller, that branch could only ever return the caller,
/// and the call site adds the caller unconditionally.
pub async fn find_affected_grocery_users(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    payload: &SyncRequest,
) -> Result<Vec<String>, AppError> {
    let list_ids: Vec<String> = payload
        .grocery_list_changes
        .iter()
        .map(|c| c.id.clone())
        .collect();
    let member_row_ids: Vec<String> = payload
        .grocery_list_member_changes
        .iter()
        .map(|c| c.id.clone())
        .collect();
    let store_ids: Vec<String> = payload.store_changes.iter().map(|c| c.id.clone()).collect();
    let category_ids: Vec<String> = payload
        .category_changes
        .iter()
        .map(|c| c.id.clone())
        .collect();
    let item_ids: Vec<String> = payload
        .grocery_changes
        .iter()
        .map(|c| c.id.clone())
        .collect();
    // Store info rows carry no list of their own; they reach a list through their store.
    let store_info_store_ids: Vec<String> = payload
        .grocery_item_store_info_changes
        .iter()
        .map(|c| c.store_id.clone())
        .collect();

    let rows = sqlx::query!(
        r#"
        WITH touched_lists AS (
            -- Every list this request named, however it named it.
            SELECT id AS list_id FROM grocery_lists WHERE id = ANY($2)
            UNION
            SELECT "listId" FROM grocery_list_members WHERE id = ANY($3)
            UNION
            SELECT "listId" FROM stores WHERE id = ANY($4) AND "listId" IS NOT NULL
            UNION
            SELECT "listId" FROM categories WHERE id = ANY($5) AND "listId" IS NOT NULL
            UNION
            SELECT "listId" FROM grocery_items WHERE id = ANY($6) AND "listId" IS NOT NULL
            UNION
            SELECT "listId" FROM stores WHERE id = ANY($7) AND "listId" IS NOT NULL
        ),
        reachable_lists AS (
            -- ...narrowed to the ones the caller is actually party to. This is what keeps a
            -- concurrent, unrelated request out of the result.
            SELECT gl.id
            FROM grocery_lists gl
            WHERE gl.id IN (SELECT list_id FROM touched_lists WHERE list_id IS NOT NULL)
              AND (
                  gl."ownerId" = $1
                  OR EXISTS (
                      SELECT 1 FROM grocery_list_members m
                      WHERE m."listId" = gl.id AND m."userId" = $1
                  )
              )
        )
        SELECT DISTINCT u.user_id AS "user_id!"
        FROM (
            -- Co-members, including ones just removed: their client has to learn of it.
            SELECT m."userId" AS user_id
            FROM grocery_list_members m
            WHERE m."listId" IN (SELECT id FROM reachable_lists)
            UNION ALL
            SELECT gl."ownerId"
            FROM grocery_lists gl
            WHERE gl.id IN (SELECT id FROM reachable_lists) AND gl."ownerId" IS NOT NULL
        ) u
        "#,
        user_id,
        &list_ids,
        &member_row_ids,
        &store_ids,
        &category_ids,
        &item_ids,
        &store_info_store_ids,
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows.into_iter().map(|r| r.user_id).collect())
}
