use axum::{
    extract::{Query, State},
    Extension, Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use redis::AsyncCommands;

use crate::state::AppState;
use crate::auth::tokens::Claims;
use crate::routes::sync::types::{AppError, SyncScope};

#[derive(Debug, Deserialize)]
pub struct SyncStatusQuery {
    pub last_synced_at: Option<DateTime<Utc>>,
    pub scope: Option<SyncScope>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncStatusResponse {
    pub needs_sync: bool,
    pub latest_version: DateTime<Utc>,
}

/// Helper function to construct the Redis key for a user's sync status.
fn get_cache_key(user_id: &str, scope: SyncScope) -> String {
    format!("user:{}:last_update:{:?}", user_id, scope)
}

/// Helper to fetch the latest updated_at timestamp from the DB for a given scope and user.
async fn get_latest_db_timestamp(
    state: &AppState,
    user_id: &str,
    scope: SyncScope,
) -> Result<DateTime<Utc>, AppError> {
    let max_updated: DateTime<Utc> = match scope {
        SyncScope::Todo => {
            sqlx::query_scalar!(
                r#"SELECT COALESCE(MAX(max_updated), '1970-01-01 00:00:00+00'::timestamptz) as "max_updated!"
                   FROM (
                       SELECT MAX(updated_at) as max_updated FROM todo_lists WHERE "userId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM todo_items WHERE "userId" = $1
                   ) subquery"#,
                user_id
            )
            .fetch_one(&state.db_pool)
            .await?
        }
        SyncScope::Grocery => {
            sqlx::query_scalar!(
                r#"SELECT COALESCE(MAX(max_updated), '1970-01-01 00:00:00+00'::timestamptz) as "max_updated!"
                   FROM (
                       SELECT MAX(updated_at) as max_updated FROM grocery_lists WHERE "ownerId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM grocery_list_members WHERE "userId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM stores WHERE "userId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM categories WHERE "userId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM grocery_items WHERE "userId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM grocery_item_store_info WHERE "userId" = $1
                   ) subquery"#,
                user_id
            )
            .fetch_one(&state.db_pool)
            .await?
        }
        SyncScope::All => {
            sqlx::query_scalar!(
                r#"SELECT COALESCE(MAX(max_updated), '1970-01-01 00:00:00+00'::timestamptz) as "max_updated!"
                   FROM (
                       SELECT MAX(updated_at) as max_updated FROM todo_lists WHERE "userId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM todo_items WHERE "userId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM grocery_lists WHERE "ownerId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM grocery_list_members WHERE "userId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM stores WHERE "userId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM categories WHERE "userId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM grocery_items WHERE "userId" = $1
                       UNION ALL
                       SELECT MAX(updated_at) as max_updated FROM grocery_item_store_info WHERE "userId" = $1
                   ) subquery"#,
                user_id
            )
            .fetch_one(&state.db_pool)
            .await?
        }
    };

    Ok(max_updated)
}

/// GET /api/sync/status
/// Checks if the client has any pending updates to sync.
pub async fn sync_status_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(query): Query<SyncStatusQuery>,
) -> Result<Json<SyncStatusResponse>, AppError> {
    let user_id = &claims.sub;
    let scope = query.scope.unwrap_or(SyncScope::All);
    let cache_key = get_cache_key(user_id, scope);

    // 1. Try fetching from Valkey cache
    let mut cached_timestamp: Option<DateTime<Utc>> = None;
    match state.redis_client.get_multiplexed_tokio_connection().await {
        Ok(mut conn) => {
            match conn.get::<_, Option<String>>(&cache_key).await {
                Ok(Some(ts_str)) => {
                    if let Ok(parsed_dt) = DateTime::parse_from_rfc3339(&ts_str) {
                        cached_timestamp = Some(parsed_dt.with_timezone(&Utc));
                    }
                }
                Ok(None) => {} // Cache miss
                Err(err) => {
                    tracing::warn!("Failed to GET key '{}' from Redis: {:?}", cache_key, err);
                }
            }
        }
        Err(err) => {
            tracing::warn!("Failed to connect to Redis: {:?}", err);
        }
    }

    // 2. Fall back to DB on cache miss
    let latest_version = match cached_timestamp {
        Some(ts) => ts,
        None => {
            let db_ts = get_latest_db_timestamp(&state, user_id, scope).await?;
            
            // Try populating cache with TTL 24 hours (86400 seconds)
            if let Ok(mut conn) = state.redis_client.get_multiplexed_tokio_connection().await {
                let ts_str = db_ts.to_rfc3339();
                if let Err(err) = conn.set_ex::<_, _, ()>(&cache_key, ts_str, 86400).await {
                    tracing::warn!("Failed to SET key '{}' with TTL: {:?}", cache_key, err);
                }
            }
            db_ts
        }
    };

    // 3. Determine if sync is needed
    let needs_sync = match query.last_synced_at {
        Some(last_synced) => latest_version > last_synced,
        None => true, // If client has never synced, it definitely needs a sync
    };

    Ok(Json(SyncStatusResponse {
        needs_sync,
        latest_version,
    }))
}
