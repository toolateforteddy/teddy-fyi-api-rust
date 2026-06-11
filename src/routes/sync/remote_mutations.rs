use super::grocery::fetch_remote_grocery_mutations;
use super::todo::fetch_remote_todo_mutations;
use super::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn fetch_remote_mutations(
    tx: &mut Transaction<'_, Postgres>,
    client_id: &str,
    last_synced_at: Option<DateTime<Utc>>,
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
    ),
    AppError,
> {
    let (remote_todo_list_changes, remote_todo_changes) =
        fetch_remote_todo_mutations(tx, client_id, last_synced_at).await?;

    let (
        remote_grocery_list_changes,
        remote_grocery_list_member_changes,
        remote_store_changes,
        remote_category_changes,
        remote_grocery_changes,
        remote_grocery_item_store_info_changes,
    ) = fetch_remote_grocery_mutations(tx, client_id, last_synced_at).await?;

    Ok((
        remote_todo_list_changes,
        remote_todo_changes,
        remote_grocery_list_changes,
        remote_grocery_list_member_changes,
        remote_store_changes,
        remote_category_changes,
        remote_grocery_changes,
        remote_grocery_item_store_info_changes,
    ))
}
