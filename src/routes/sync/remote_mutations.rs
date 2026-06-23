use super::grocery::fetch_remote_grocery_mutations;
use super::todo::fetch_remote_todo_mutations;
use super::config::fetch_remote_config_mutations;
use super::drawing::fetch_remote_drawing_mutations;
use super::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

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

    let remote_config_changes = if scope == SyncScope::ScribbleBox
        || scope == SyncScope::ScribbleKeep
        || scope == SyncScope::ScribbleKeepCloud
    {
        let user_uuid = parse_or_hash_uuid(user_id);
        let client_uuid = parse_or_hash_uuid(client_id);
        fetch_remote_config_mutations(tx, &user_uuid, &client_uuid, last_synced_at).await?
    } else {
        vec![]
    };

    let remote_drawing_changes = if scope == SyncScope::ScribbleKeepCloud {
        let user_uuid = parse_or_hash_uuid(user_id);
        let client_uuid = parse_or_hash_uuid(client_id);
        fetch_remote_drawing_mutations(tx, &user_uuid, &client_uuid, last_synced_at).await?
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
