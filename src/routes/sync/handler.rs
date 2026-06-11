use axum::extract::{State, Json};
use chrono::Utc;
use sqlx::{Postgres, Transaction};
use crate::state::AppState;
use super::types::*;
use super::todo::*;
use super::grocery::*;
use super::remote_mutations::*;

pub async fn sync_handler(
    State(state): State<AppState>,
    Json(payload): Json<SyncRequest>,
) -> Result<Json<SyncResponse>, AppError> {
    let mut tx: Transaction<'_, Postgres> = state.db_pool.begin().await?;
    let server_timestamp = Utc::now();
    let mut success_ids = Vec::new();

    // Process todo_list_changes first as todo_items reference todo_lists
    process_todo_list_changes(&mut tx, &payload.client_id, server_timestamp, &payload.todo_list_changes, &mut success_ids).await?;
    process_todo_changes(&mut tx, &payload.client_id, server_timestamp, &payload.todo_changes, &mut success_ids).await?;

    // Process grocery_lists, grocery_list_members, stores, categories first, then grocery_items and grocery_item_store_info
    process_grocery_list_changes(&mut tx, &payload.client_id, server_timestamp, &payload.grocery_list_changes, &mut success_ids).await?;
    process_grocery_list_member_changes(&mut tx, &payload.client_id, server_timestamp, &payload.grocery_list_member_changes, &mut success_ids).await?;
    process_store_changes(&mut tx, &payload.client_id, server_timestamp, &payload.store_changes, &mut success_ids).await?;
    process_category_changes(&mut tx, &payload.client_id, server_timestamp, &payload.category_changes, &mut success_ids).await?;
    process_grocery_changes(&mut tx, &payload.client_id, server_timestamp, &payload.grocery_changes, &mut success_ids).await?;
    process_grocery_item_store_info_changes(&mut tx, &payload.client_id, server_timestamp, &payload.grocery_item_store_info_changes, &mut success_ids).await?;

    // Fetch remote mutations
    let (
        remote_todo_list_changes,
        remote_todo_changes,
        remote_grocery_list_changes,
        remote_grocery_list_member_changes,
        remote_store_changes,
        remote_category_changes,
        remote_grocery_changes,
        remote_grocery_item_store_info_changes,
    ) = fetch_remote_mutations(&mut tx, &payload.client_id, payload.last_synced_at).await?;

    // Commit transaction
    tx.commit().await?;

    Ok(Json(SyncResponse {
        success_ids,
        remote_todo_list_changes,
        remote_todo_changes,
        remote_grocery_list_changes,
        remote_grocery_list_member_changes,
        remote_store_changes,
        remote_category_changes,
        remote_grocery_changes,
        remote_grocery_item_store_info_changes,
        server_timestamp,
    }))
}
