use super::grocery::fetch_remote_grocery_mutations;
use super::todo::fetch_remote_todo_mutations;
use super::config::fetch_config_download;
use super::drawing::fetch_drawing_download;
use super::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

/// Maps an identifier string onto a UUID: parsed if it already is one, otherwise hashed
/// with `uuid5(NAMESPACE_DNS, s)`.
///
/// **This is one of two user identities in this service, and changing it is a data
/// migration, not a refactor.** `configs`, `drawings` and `devices` are keyed by the UUID
/// this returns for the auth subject; `todo_*`, `grocery_*`, `users`, `sessions` and
/// `list_invites` are keyed by that same subject *raw*, as text. Nothing stores the
/// mapping — it is recomputed on every request — so a different namespace, hash or input
/// encoding orphans every config, drawing and device row that exists. The reasoning, the
/// full table-by-table split and what a re-key would cost are in
/// `context/2026-09-05_user_identity_derivation.md`.
///
/// Two properties worth knowing before you rely on this:
///
/// - **The output is publicly computable.** `uuid5` is unkeyed and the auth subject is not
///   a secret (co-members of a shared grocery list receive each other's raw subject in
///   `grocery_list_members` rows), so anyone can derive another account's config UUID.
///   That is not itself an access grant: every query scopes by the UUID derived from the
///   *caller's own* verified claims, never from anything in the request body. It does mean
///   the value must never be treated as a capability, secret, or proof of ownership.
/// - **The two branches share one output space.** A subject that is already UUID-shaped is
///   used verbatim, so it can name the same identifier a hashed subject derives. Real
///   Google `sub` values are decimal digit strings and can never take that shape; only a
///   path that lets a caller choose its own subject can reach it.
///
/// Also used for `client_id`, where the same determinism is all that is wanted and none of
/// the above is load-bearing — it is an echo-suppression tag, not an identity.
pub fn parse_or_hash_uuid(s: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(s).unwrap_or_else(|_| {
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, s.as_bytes())
    })
}

pub async fn fetch_remote_mutations(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    last_synced_at: Option<DateTime<Utc>>,
    scope: SyncScope,
) -> Result<
    (
        Vec<TodoListChangeDelta>,
        Vec<TodoChangeDelta>,
        Vec<GroceryListChangeDelta>,
        Vec<GroceryListMemberChangeDelta>,
        Vec<StoreChangeDelta>,
        Vec<CategoryChangeDelta>,
        Vec<GroceryChangeDelta>,
        Vec<GroceryItemStoreInfoChangeDelta>,
        Vec<ConfigChangeDelta>,
        Vec<DrawingChangeDelta>,
    ),
    AppError,
> {
    let (remote_todo_list_changes, remote_todo_changes) = if scope == SyncScope::All || scope == SyncScope::Todo {
        fetch_remote_todo_mutations(tx, user_id, client_id, last_synced_at).await?
    } else {
        (vec![], vec![])
    };

    let (
        remote_grocery_list_changes,
        remote_grocery_list_member_changes,
        remote_store_changes,
        remote_category_changes,
        remote_grocery_changes,
        remote_grocery_item_store_info_changes,
    ) = if scope == SyncScope::All || scope == SyncScope::Grocery {
        fetch_remote_grocery_mutations(tx, user_id, client_id, last_synced_at).await?
    } else {
        (vec![], vec![], vec![], vec![], vec![], vec![])
    };

    // Unpaged, deliberately. This path keeps only `remote_changes` and drops the
    // `next_cursor_ms` that says a page was cut short, so a bound here would truncate with
    // nothing to tell the client where to resume. The handler calls the downloads directly
    // and does carry the cursor; this helper does not.
    let remote_config_changes = if scope == SyncScope::ScribbleBox
        || scope == SyncScope::ScribbleKeep
        || scope == SyncScope::ScribbleKeepCloud
    {
        let user_uuid = parse_or_hash_uuid(user_id);
        let client_uuid = parse_or_hash_uuid(client_id);
        fetch_config_download(tx, &user_uuid, &client_uuid, None, last_synced_at, None)
            .await?
            .remote_changes
    } else {
        vec![]
    };

    let remote_drawing_changes = if scope == SyncScope::ScribbleKeepCloud {
        let user_uuid = parse_or_hash_uuid(user_id);
        let client_uuid = parse_or_hash_uuid(client_id);
        fetch_drawing_download(tx, &user_uuid, &client_uuid, None, last_synced_at, None)
            .await?
            .remote_changes
    } else {
        vec![]
    };

    Ok((
        remote_todo_list_changes,
        remote_todo_changes,
        remote_grocery_list_changes,
        remote_grocery_list_member_changes,
        remote_store_changes,
        remote_category_changes,
        remote_grocery_changes,
        remote_grocery_item_store_info_changes,
        remote_config_changes,
        remote_drawing_changes,
    ))
}
