use super::grocery::*;
use super::remote_mutations::*;
use super::todo::*;
use super::types::*;
use super::config::*;
use super::drawing::*;
use crate::state::AppState;
use crate::auth::tokens::Claims;
use axum::{
    extract::{Json, State},
    Extension,
};
use chrono::Utc;
use sqlx::{Postgres, Transaction};
use redis::AsyncCommands;

pub async fn sync_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AppJson(payload): AppJson<SyncRequest>,
) -> Result<Json<SyncResponse>, AppError> {
    let mut tx: Transaction<'_, Postgres> = state.db_pool.begin().await?;
    let server_timestamp = Utc::now();
    let mut remote_todo_list_changes = Vec::new();
    let mut remote_todo_changes = Vec::new();
    let mut remote_grocery_list_changes = Vec::new();
    let mut remote_grocery_list_member_changes = Vec::new();
    let mut remote_store_changes = Vec::new();
    let mut remote_category_changes = Vec::new();
    let mut remote_grocery_changes = Vec::new();
    let mut remote_grocery_item_store_info_changes = Vec::new();
    let mut remote_config_changes = Vec::new();
    let mut remote_drawing_changes = Vec::new();
    let mut success_ids = Vec::new();
    let mut upload_status = Vec::new();
    let mut success_config_uuids = Vec::new();
    let mut success_drawing_uuids = Vec::new();

    let scope = payload.scope.unwrap_or(SyncScope::All);

    tracing::info!(
        "Incoming sync request: client_id={}, scope={:?}, config_changes={}, drawing_changes={}, configs={}, drawings={}, todo_changes={}, grocery_changes={}",
        payload.client_id,
        scope,
        payload.config_changes.len(),
        payload.drawing_changes.len(),
        payload.configs.len(),
        payload.drawings.len(),
        payload.todo_changes.len(),
        payload.grocery_changes.len()
    );

    match scope {
        SyncScope::ScribbleBox => {
            let user_uuid = parse_or_hash_uuid(&claims.sub);
            let client_uuid = parse_or_hash_uuid(&payload.client_id);
            
            if !payload.drawings.is_empty() {
                process_drawing_sync_items(
                    &mut tx,
                    &user_uuid,
                    &client_uuid,
                    &payload.drawings,
                    &mut success_drawing_uuids,
                )
                .await?;
                for uuid in &success_drawing_uuids {
                    success_ids.push(uuid.to_string());
                }
            }
            if !payload.drawing_changes.is_empty() {
                process_drawing_changes(
                    &mut tx,
                    &user_uuid,
                    &client_uuid,
                    &payload.drawing_changes,
                    &mut success_ids,
                    &mut upload_status,
                    &mut remote_drawing_changes,
                )
                .await?;
            }
        }
        SyncScope::ScribbleKeep | SyncScope::ScribbleKeepCloud => {
            let user_uuid = parse_or_hash_uuid(&claims.sub);
            let client_uuid = parse_or_hash_uuid(&payload.client_id);

            if !payload.configs.is_empty() {
                process_config_sync_items(
                    &mut tx,
                    &user_uuid,
                    &client_uuid,
                    &payload.configs,
                    &mut success_config_uuids,
                )
                .await?;
                for uuid in &success_config_uuids {
                    success_ids.push(uuid.to_string());
                }
            }
            if !payload.config_changes.is_empty() {
                process_config_changes(
                    &mut tx,
                    &user_uuid,
                    &client_uuid,
                    &payload.config_changes,
                    &mut success_ids,
                    &mut upload_status,
                    &mut remote_config_changes,
                )
                .await?;
            }
        }
        _ => {
            // Process todo_list_changes first as todo_items reference todo_lists
            process_todo_list_changes(
                &mut tx,
                &claims.sub,
                &payload.client_id,
                server_timestamp,
                &payload.todo_list_changes,
                &mut success_ids,
                &mut upload_status,
                &mut remote_todo_list_changes,
            )
            .await?;
            process_todo_changes(
                &mut tx,
                &claims.sub,
                &payload.client_id,
                &state.gemini_api_key,
                server_timestamp,
                &payload.todo_changes,
                &mut success_ids,
                &mut upload_status,
                &mut remote_todo_changes,
            )
            .await?;

            // Process grocery_lists, grocery_list_members, stores, categories first, then grocery_items and grocery_item_store_info
            process_grocery_list_changes(
                &mut tx,
                &claims.sub,
                &payload.client_id,
                server_timestamp,
                &payload.grocery_list_changes,
                &mut success_ids,
                &mut upload_status,
                &mut remote_grocery_list_changes,
            )
            .await?;
            process_grocery_list_member_changes(
                &mut tx,
                &claims.sub,
                &payload.client_id,
                server_timestamp,
                &payload.grocery_list_member_changes,
                &mut success_ids,
                &mut upload_status,
                &mut remote_grocery_list_member_changes,
            )
            .await?;
            process_store_changes(
                &mut tx,
                &claims.sub,
                &payload.client_id,
                server_timestamp,
                &payload.store_changes,
                &mut success_ids,
                &mut upload_status,
                &mut remote_store_changes,
            )
            .await?;
            process_category_changes(
                &mut tx,
                &claims.sub,
                &payload.client_id,
                server_timestamp,
                &payload.category_changes,
                &mut success_ids,
                &mut upload_status,
                &mut remote_category_changes,
            )
            .await?;
            process_grocery_changes(
                &mut tx,
                &claims.sub,
                &payload.client_id,
                server_timestamp,
                &payload.grocery_changes,
                &mut success_ids,
                &mut upload_status,
                &mut remote_grocery_changes,
            )
            .await?;
            process_grocery_item_store_info_changes(
                &mut tx,
                &claims.sub,
                &payload.client_id,
                server_timestamp,
                &payload.grocery_item_store_info_changes,
                &mut success_ids,
                &mut upload_status,
                &mut remote_grocery_item_store_info_changes,
            )
            .await?;
        }
    }

    // Fetch remote mutations
    let (
        fetched_todo_list,
        fetched_todo,
        fetched_grocery_list,
        fetched_grocery_list_member,
        fetched_store,
        fetched_category,
        fetched_grocery,
        fetched_grocery_item_store_info,
        fetched_config,
        fetched_drawing,
    ) = fetch_remote_mutations(
        &mut tx,
        &claims.sub,
        &payload.client_id,
        payload.last_synced_at,
        scope,
    )
    .await?;

    {
        use std::collections::HashSet;
        
        let existing_todo_list_ids: HashSet<String> = remote_todo_list_changes.iter().map(|c| c.id.clone()).collect();
        remote_todo_list_changes.extend(fetched_todo_list.into_iter().filter(|c| !existing_todo_list_ids.contains(&c.id)));

        let existing_todo_ids: HashSet<String> = remote_todo_changes.iter().map(|c| c.id.clone()).collect();
        remote_todo_changes.extend(fetched_todo.into_iter().filter(|c| !existing_todo_ids.contains(&c.id)));

        let existing_grocery_list_ids: HashSet<String> = remote_grocery_list_changes.iter().map(|c| c.id.clone()).collect();
        remote_grocery_list_changes.extend(fetched_grocery_list.into_iter().filter(|c| !existing_grocery_list_ids.contains(&c.id)));

        let existing_grocery_list_member_ids: HashSet<String> = remote_grocery_list_member_changes.iter().map(|c| c.id.clone()).collect();
        remote_grocery_list_member_changes.extend(fetched_grocery_list_member.into_iter().filter(|c| !existing_grocery_list_member_ids.contains(&c.id)));

        let existing_store_ids: HashSet<String> = remote_store_changes.iter().map(|c| c.id.clone()).collect();
        remote_store_changes.extend(fetched_store.into_iter().filter(|c| !existing_store_ids.contains(&c.id)));

        let existing_category_ids: HashSet<String> = remote_category_changes.iter().map(|c| c.id.clone()).collect();
        remote_category_changes.extend(fetched_category.into_iter().filter(|c| !existing_category_ids.contains(&c.id)));

        let existing_grocery_ids: HashSet<String> = remote_grocery_changes.iter().map(|c| c.id.clone()).collect();
        remote_grocery_changes.extend(fetched_grocery.into_iter().filter(|c| !existing_grocery_ids.contains(&c.id)));

        let existing_grocery_item_store_info_ids: HashSet<String> = remote_grocery_item_store_info_changes.iter().map(|c| c.id.clone()).collect();
        remote_grocery_item_store_info_changes.extend(fetched_grocery_item_store_info.into_iter().filter(|c| !existing_grocery_item_store_info_ids.contains(&c.id)));

        let existing_config_ids: HashSet<String> = remote_config_changes.iter().map(|c| c.id.clone()).collect();
        remote_config_changes.extend(fetched_config.into_iter().filter(|c| !existing_config_ids.contains(&c.id)));

        let existing_drawing_ids: HashSet<String> = remote_drawing_changes.iter().map(|c| c.id.clone()).collect();
        remote_drawing_changes.extend(fetched_drawing.into_iter().filter(|c| !existing_drawing_ids.contains(&c.id)));
    }

    let user_uuid = parse_or_hash_uuid(&claims.sub);
    let client_uuid = parse_or_hash_uuid(&payload.client_id);

    let response_configs = if scope == SyncScope::ScribbleBox
        || scope == SyncScope::ScribbleKeep
        || scope == SyncScope::ScribbleKeepCloud
    {
        fetch_configs_for_response(
            &mut tx,
            &user_uuid,
            &client_uuid,
            payload.last_synced_at,
            &success_config_uuids,
        )
        .await?
    } else {
        vec![]
    };

    let response_drawings = if scope == SyncScope::ScribbleKeepCloud || (scope == SyncScope::ScribbleBox && !success_drawing_uuids.is_empty()) {
        fetch_drawings_for_response(
            &mut tx,
            &user_uuid,
            &client_uuid,
            payload.last_synced_at,
            &success_drawing_uuids,
            scope == SyncScope::ScribbleKeepCloud,
        )
        .await?
    } else {
        vec![]
    };

    let has_grocery = !payload.grocery_list_changes.is_empty()
        || !payload.grocery_list_member_changes.is_empty()
        || !payload.store_changes.is_empty()
        || !payload.category_changes.is_empty()
        || !payload.grocery_changes.is_empty()
        || !payload.grocery_item_store_info_changes.is_empty();

    let mut affected_grocery_users = Vec::new();
    if has_grocery {
        let rows = sqlx::query!(
            r#"
            SELECT DISTINCT "userId" as user_id FROM (
                -- Users who are members of the updated lists
                SELECT glm."userId"
                FROM grocery_list_members glm
                WHERE glm."listId" IN (
                    SELECT DISTINCT "listId" FROM (
                        SELECT id as "listId" FROM grocery_lists WHERE updated_at = $1
                        UNION ALL
                        SELECT "listId" FROM grocery_list_members WHERE updated_at = $1 AND "listId" IS NOT NULL
                        UNION ALL
                        SELECT "listId" FROM stores WHERE updated_at = $1 AND "listId" IS NOT NULL
                        UNION ALL
                        SELECT "listId" FROM categories WHERE updated_at = $1 AND "listId" IS NOT NULL
                        UNION ALL
                        SELECT "listId" FROM grocery_items WHERE updated_at = $1 AND "listId" IS NOT NULL
                        UNION ALL
                        SELECT s."listId" FROM grocery_item_store_info gsi
                        JOIN stores s ON gsi."storeId" = s.id
                        WHERE gsi.updated_at = $1 AND s."listId" IS NOT NULL
                    ) sub_lists
                )
                UNION ALL
                -- Owners of updated lists
                SELECT "ownerId" as "userId" FROM grocery_lists WHERE updated_at = $1 AND "ownerId" IS NOT NULL
                UNION ALL
                -- Users who own updated items/stores/categories with no list
                SELECT "userId" FROM grocery_items WHERE updated_at = $1 AND "listId" IS NULL AND "userId" IS NOT NULL
                UNION ALL
                SELECT "userId" FROM stores WHERE updated_at = $1 AND "listId" IS NULL AND "userId" IS NOT NULL
                UNION ALL
                SELECT "userId" FROM categories WHERE updated_at = $1 AND "listId" IS NULL AND "userId" IS NOT NULL
            ) all_users
            "#,
            server_timestamp
        )
        .fetch_all(&mut *tx)
        .await?;

        for r in rows {
            if let Some(uid) = r.user_id {
                affected_grocery_users.push(uid);
            }
        }
    }

    // Commit transaction
    tx.commit().await?;

    let has_mutations = !payload.todo_list_changes.is_empty()
        || !payload.todo_changes.is_empty()
        || has_grocery
        || !payload.config_changes.is_empty()
        || !payload.drawing_changes.is_empty()
        || !payload.configs.is_empty()
        || !payload.drawings.is_empty();

    if has_mutations {
        if let Ok(mut conn) = state.redis_client.get_multiplexed_tokio_connection().await {
            let ts_str = server_timestamp.to_rfc3339();
            
            // Update All scope for the requesting user
            let _ = conn.set_ex::<_, _, ()>(&format!("user:{}:last_update:All", claims.sub), &ts_str, 86400).await;

            // Invalidate/update caches for all members/collaborators of the updated grocery lists
            if has_grocery {
                if !affected_grocery_users.contains(&claims.sub) {
                    affected_grocery_users.push(claims.sub.clone());
                }
                for user_id in &affected_grocery_users {
                    let _ = conn.set_ex::<_, _, ()>(&format!("user:{}:last_update:Grocery", user_id), &ts_str, 86400).await;
                    let _ = conn.set_ex::<_, _, ()>(&format!("user:{}:last_update:All", user_id), &ts_str, 86400).await;
                }
            }

            // Update specific scopes for the requesting user
            let has_todo = !payload.todo_list_changes.is_empty() || !payload.todo_changes.is_empty();
            let has_scribble_box = !payload.drawing_changes.is_empty() || !payload.drawings.is_empty();
            let has_scribble_keep = !payload.config_changes.is_empty() || !payload.configs.is_empty();

            if has_todo {
                let _ = conn.set_ex::<_, _, ()>(&format!("user:{}:last_update:Todo", claims.sub), &ts_str, 86400).await;
            }
            if has_scribble_box {
                let _ = conn.set_ex::<_, _, ()>(&format!("user:{}:last_update:ScribbleBox", claims.sub), &ts_str, 86400).await;
            }
            if has_scribble_keep {
                let _ = conn.set_ex::<_, _, ()>(&format!("user:{}:last_update:ScribbleKeep", claims.sub), &ts_str, 86400).await;
                let _ = conn.set_ex::<_, _, ()>(&format!("user:{}:last_update:ScribbleKeepCloud", claims.sub), &ts_str, 86400).await;
            }
        }
    }

    let client_uuid = parse_or_hash_uuid(&payload.client_id);
    tracing::info!(
        "Sync successful for client ID {} (UUID: {}) with scope {:?}",
        payload.client_id,
        client_uuid,
        scope
    );

    Ok(Json(SyncResponse {
        success_ids,
        upload_status,
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
        configs: response_configs,
        drawings: response_drawings,
        server_timestamp,
    }))
}
