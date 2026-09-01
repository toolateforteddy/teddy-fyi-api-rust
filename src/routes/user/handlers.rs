use axum::{extract::State, Extension, Json};
use serde::{Deserialize, Serialize};
use crate::auth::tokens::Claims;
use crate::routes::sync::publisher::{publish_user_event, SyncSseEvent};
use crate::routes::sync::remote_mutations::parse_or_hash_uuid;
use crate::routes::sync::types::AppError;
use crate::state::AppState;

/// Row counts removed per table, so a caller (or an audit log) can see exactly what the
/// deletion touched rather than trusting a bare 200.
#[derive(Debug, Default, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct DeletedCounts {
    pub todo_lists: u64,
    pub todo_items: u64,
    pub grocery_lists: u64,
    pub grocery_list_members: u64,
    pub grocery_items: u64,
    pub grocery_item_store_info: u64,
    pub stores: u64,
    pub categories: u64,
    pub list_invites: u64,
    pub configs: u64,
    pub drawings: u64,
    pub devices: u64,
    pub sessions: u64,
    pub users: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeleteUserDataResponse {
    pub user_id: String,
    pub deleted: DeletedCounts,
}

/// `DELETE /api/user/data` — erase everything the authenticated account owns.
///
/// Scoped entirely to the caller's own claims: there is no way to name another user.
/// Grocery lists the caller owns go with them, which also removes them for anyone they
/// shared those lists with; lists merely *joined* survive and only lose this member.
/// The whole erase runs in one transaction, so a failure anywhere leaves the account intact.
pub async fn delete_user_data_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<DeleteUserDataResponse>, AppError> {
    let user_id = claims.sub.clone();
    let user_uuid = parse_or_hash_uuid(&user_id);

    let mut tx = state.db_pool.begin().await?;

    // Lists the caller owns are deleted outright. Their child rows do not all cascade
    // (`grocery_items`/`stores`/`categories` are ON DELETE SET NULL), so they are removed
    // explicitly below before the lists go.
    let owned_lists: Vec<String> = sqlx::query_scalar!(
        r#"SELECT id FROM grocery_lists WHERE "ownerId" = $1"#,
        user_id
    )
    .fetch_all(&mut *tx)
    .await?;

    // Co-members of those lists lose data they can see, so their cached sync watermark has
    // to be bumped once the transaction commits.
    let affected_users: Vec<String> = sqlx::query_scalar!(
        r#"SELECT DISTINCT "userId" FROM grocery_list_members WHERE "listId" = ANY($1) AND "userId" <> $2"#,
        &owned_lists,
        user_id
    )
    .fetch_all(&mut *tx)
    .await?;

    let grocery_item_store_info = sqlx::query!(
        r#"DELETE FROM grocery_item_store_info
           WHERE "userId" = $1
              OR "groceryItemId" IN (SELECT id FROM grocery_items WHERE "listId" = ANY($2))
              OR "storeId" IN (SELECT id FROM stores WHERE "listId" = ANY($2))"#,
        user_id,
        &owned_lists
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let grocery_items = sqlx::query!(
        r#"DELETE FROM grocery_items WHERE "userId" = $1 OR "listId" = ANY($2)"#,
        user_id,
        &owned_lists
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let stores = sqlx::query!(
        r#"DELETE FROM stores WHERE "userId" = $1 OR "listId" = ANY($2)"#,
        user_id,
        &owned_lists
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let categories = sqlx::query!(
        r#"DELETE FROM categories WHERE "userId" = $1 OR "listId" = ANY($2)"#,
        user_id,
        &owned_lists
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let grocery_list_members = sqlx::query!(
        r#"DELETE FROM grocery_list_members WHERE "userId" = $1 OR "listId" = ANY($2)"#,
        user_id,
        &owned_lists
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let list_invites = sqlx::query!(
        r#"DELETE FROM list_invites WHERE "createdBy" = $1 OR "listId" = ANY($2)"#,
        user_id,
        &owned_lists
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let grocery_lists = sqlx::query!(
        r#"DELETE FROM grocery_lists WHERE "ownerId" = $1"#,
        user_id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let todo_items = sqlx::query!(
        r#"DELETE FROM todo_items WHERE "userId" = $1"#,
        user_id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    let todo_lists = sqlx::query!(
        r#"DELETE FROM todo_lists WHERE "userId" = $1"#,
        user_id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();

    // `configs`, `drawings` and `devices` key off the UUID derived from the auth subject
    // rather than the subject itself. See `parse_or_hash_uuid`.
    let configs = sqlx::query!("DELETE FROM configs WHERE user_id = $1", user_uuid)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    let drawings = sqlx::query!("DELETE FROM drawings WHERE user_id = $1", user_uuid)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    let devices = sqlx::query!("DELETE FROM devices WHERE user_id = $1", user_uuid)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    // Sessions last but one: dropping them logs every client of the account out, and the
    // access token the caller used stops refreshing.
    let sessions = sqlx::query!("DELETE FROM sessions WHERE user_id = $1", user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    let users = sqlx::query!("DELETE FROM users WHERE id = $1", user_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

    let deleted = DeletedCounts {
        todo_lists,
        todo_items,
        grocery_lists,
        grocery_list_members,
        grocery_items,
        grocery_item_store_info,
        stores,
        categories,
        list_invites,
        configs,
        drawings,
        devices,
        sessions,
        users,
    };

    tx.commit().await?;

    tracing::info!(user_id = %user_id, deleted = ?deleted, "Deleted all data for user");

    invalidate_caches(&state, &user_id, &affected_users).await;

    // Tell any live SSE listener to drop what it has. A Redis outage must not fail a
    // deletion that already committed, so failures only log.
    let event = SyncSseEvent::Invalidate {
        entity: "all".to_string(),
        sender_client_id: None,
        device_uuid: None,
    };
    if let Err(err) = publish_user_event(&state.redis_client, &user_id, &event).await {
        tracing::warn!("Failed to publish deletion event for user {}: {:?}", user_id, err);
    }

    Ok(Json(DeleteUserDataResponse { user_id, deleted }))
}

/// Drops the deleted user's cached sync watermarks and bumps them for anyone who shared a
/// list with them, so collaborators notice the rows that just disappeared.
async fn invalidate_caches(state: &AppState, user_id: &str, affected_users: &[String]) {
    use redis::AsyncCommands;

    let mut conn = match state.redis_client.get_multiplexed_tokio_connection().await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!("Failed to reach Redis while deleting user data: {:?}", err);
            return;
        }
    };

    const SCOPES: [&str; 6] = [
        "All",
        "Grocery",
        "Todo",
        "ScribbleBox",
        "ScribbleKeep",
        "ScribbleKeepCloud",
    ];

    for scope in SCOPES {
        let key = format!("user:{}:last_update:{}", user_id, scope);
        if let Err(err) = conn.del::<_, ()>(&key).await {
            tracing::warn!("Failed to DEL key '{}': {:?}", key, err);
        }
    }

    let ts_str = chrono::Utc::now().to_rfc3339();
    for other in affected_users {
        for scope in ["All", "Grocery"] {
            let key = format!("user:{}:last_update:{}", other, scope);
            if let Err(err) = conn.set_ex::<_, _, ()>(&key, &ts_str, 86400).await {
                tracing::warn!("Failed to SET key '{}': {:?}", key, err);
            }
        }
    }
}
