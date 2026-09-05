use super::grocery::*;
use super::remote_mutations::*;
use super::todo::*;
use super::types::*;
use super::config::*;
use super::device::*;
use super::limits::{validate_sync_payload, SyncLimits};
use super::drawing::*;
use super::publisher::{publish_device_event, SyncSseEvent};
use crate::state::AppState;
use crate::auth::tokens::Claims;
use axum::{
    extract::{Json, State},
    Extension,
};
use chrono::Utc;
use redis::AsyncCommands;

pub async fn sync_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    AppJson(payload): AppJson<SyncRequest>,
) -> Result<Json<SyncResponse>, AppError> {
    let server_timestamp = Utc::now();
    // The one clock reading this request is written with. Everything stamped below shares
    // it, and it is the same instant the response reports back, so a client that stores
    // `server_timestamp` as its cursor cannot end up either re-reading or skipping the
    // rows this very request wrote. See `crate::routes::sync::versioning`.
    let server_ms = server_timestamp.timestamp_millis();
    let scope = payload.scope.unwrap_or(SyncScope::All);

    // Bounds first, before a transaction is opened or a row is touched: an over-large
    // drawing blob or an over-long config value fails the whole request with a 400 that
    // names the field, and nothing is written. See `crate::routes::sync::limits`.
    validate_sync_payload(&payload, &SyncLimits::from_env())?;

    // The three futures below each read the whole request. Share one allocation rather than
    // deep-copying a body that is mostly drawing vector data. See `SharedRequest`.
    let payload = SharedRequest::new(payload);

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

    // 1. Todo Future
    let todo_future = {
        let state = state.clone();
        let claims = claims.clone();
        let payload = payload.handle();
        async move {
            if scope == SyncScope::All || scope == SyncScope::Todo {
                let mut tx = state.db_pool.begin().await?;
                let mut success_ids = Vec::new();
                let mut upload_status = Vec::new();
                let mut remote_todo_list_changes = Vec::new();
                let mut remote_todo_changes = Vec::new();

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
                    &state,
                    server_timestamp,
                    &payload.todo_changes,
                    &mut success_ids,
                    &mut upload_status,
                    &mut remote_todo_changes,
                )
                .await?;

                let (fetched_todo_list, fetched_todo) = fetch_remote_todo_mutations(
                    &mut tx,
                    &claims.sub,
                    &payload.client_id,
                    payload.last_synced_at,
                )
                .await?;

                tx.commit().await?;

                // Merge fetched changes into remote mutations
                {
                    use std::collections::HashSet;
                    let existing_todo_list_ids: HashSet<String> = remote_todo_list_changes.iter().map(|c| c.id.clone()).collect();
                    remote_todo_list_changes.extend(fetched_todo_list.into_iter().filter(|c| !existing_todo_list_ids.contains(&c.id)));

                    let existing_todo_ids: HashSet<String> = remote_todo_changes.iter().map(|c| c.id.clone()).collect();
                    remote_todo_changes.extend(fetched_todo.into_iter().filter(|c| !existing_todo_ids.contains(&c.id)));
                }

                Ok::<_, AppError>((success_ids, upload_status, remote_todo_list_changes, remote_todo_changes))
            } else {
                Ok((vec![], vec![], vec![], vec![]))
            }
        }
    };

    // 2. Grocery Future
    let grocery_future = {
        let state = state.clone();
        let claims = claims.clone();
        let payload = payload.handle();
        async move {
            if scope == SyncScope::All || scope == SyncScope::Grocery {
                let mut tx = state.db_pool.begin().await?;
                let mut success_ids = Vec::new();
                let mut upload_status = Vec::new();
                let mut remote_grocery_list_changes = Vec::new();
                let mut remote_grocery_list_member_changes = Vec::new();
                let mut remote_store_changes = Vec::new();
                let mut remote_category_changes = Vec::new();
                let mut remote_grocery_changes = Vec::new();
                let mut remote_grocery_item_store_info_changes = Vec::new();

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

                let (
                    fetched_grocery_list,
                    fetched_grocery_list_member,
                    fetched_store,
                    fetched_category,
                    fetched_grocery,
                    fetched_grocery_item_store_info,
                ) = fetch_remote_grocery_mutations(
                    &mut tx,
                    &claims.sub,
                    &payload.client_id,
                    payload.last_synced_at,
                )
                .await?;

                // Merge fetched changes into remote mutations
                {
                    use std::collections::HashSet;
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
                }

                // Check affected grocery users before committing
                let has_grocery = !payload.grocery_list_changes.is_empty()
                    || !payload.grocery_list_member_changes.is_empty()
                    || !payload.store_changes.is_empty()
                    || !payload.category_changes.is_empty()
                    || !payload.grocery_changes.is_empty()
                    || !payload.grocery_item_store_info_changes.is_empty();

                // Bump the grocery caches of everyone who can see a list this request
                // touched, not just the caller — that is what makes a shared list show up on
                // a co-member's device. Resolved from the ids the request named, inside the
                // still-open transaction so the rows just written are visible.
                let affected_grocery_users = if has_grocery {
                    find_affected_grocery_users(&mut tx, &claims.sub, &payload).await?
                } else {
                    Vec::new()
                };

                tx.commit().await?;

                Ok::<_, AppError>((
                    success_ids,
                    upload_status,
                    remote_grocery_list_changes,
                    remote_grocery_list_member_changes,
                    remote_store_changes,
                    remote_category_changes,
                    remote_grocery_changes,
                    remote_grocery_item_store_info_changes,
                    affected_grocery_users,
                ))
            } else {
                Ok((vec![], vec![], vec![], vec![], vec![], vec![], vec![], vec![], vec![]))
            }
        }
    };

    // 3. Config & Drawing Future
    let config_drawing_future = {
        let state = state.clone();
        let claims = claims.clone();
        let payload = payload.handle();
        async move {
            if scope == SyncScope::ScribbleBox
                || scope == SyncScope::ScribbleKeep
                || scope == SyncScope::ScribbleKeepCloud
            {
                let mut tx = state.db_pool.begin().await?;
                let mut success_ids = Vec::new();
                let mut upload_status = Vec::new();
                let mut remote_config_changes = Vec::new();
                let mut remote_drawing_changes = Vec::new();
                let mut success_config_uuids = Vec::new();
                let mut config_broadcasts = Vec::new();
                let mut success_drawing_uuids = Vec::new();

                let user_uuid = parse_or_hash_uuid(&claims.sub);
                let client_uuid = parse_or_hash_uuid(&payload.client_id);

                // A tablet scope speaks for its own device: it registers itself on first
                // sync, and a request without a device_uuid falls back to the account's
                // backfilled one.
                //
                // The cloud app does not. It runs on a machine that is not one of these
                // tablets and edits whichever device the user picked from its dropdown, so
                // it registers nothing, claims no device of its own, and names its subject
                // in each row instead. Any device_uuid on the request itself would be the
                // host it happens to run on, which is not what is being written to.
                let request_device = if scope == SyncScope::ScribbleKeepCloud {
                    tracing::debug!(
                        "Cloud sync for user {} from client {}; device comes from each row",
                        user_uuid,
                        client_uuid
                    );
                    None
                } else {
                    Some(
                        resolve_sync_device(
                            &mut tx,
                            &user_uuid,
                            payload.device_uuid,
                            payload.device_name.as_deref(),
                        )
                        .await?,
                    )
                };

                // ScribbleBox and ScribbleKeep see only their own tablet; the cloud app
                // reads across every device on the account.
                let device_filter = request_device;
                let device_rule = match request_device {
                    Some(device) => ItemDeviceRule::RequestDevice(device),
                    None => ItemDeviceRule::RowMustName,
                };

                // Drawings are uploaded under ScribbleBox (legacy) and ScribbleKeep (current).
                let uploads_drawings =
                    scope == SyncScope::ScribbleBox || scope == SyncScope::ScribbleKeep;
                // Configs are uploaded under the Keep scopes.
                let uploads_configs = scope == SyncScope::ScribbleKeep
                    || scope == SyncScope::ScribbleKeepCloud;

                if uploads_drawings {
                    if !payload.drawings.is_empty() {
                        process_drawing_sync_items(
                            &mut tx,
                            &user_uuid,
                            &client_uuid,
                            server_ms,
                            &device_rule,
                            device_filter,
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
                            server_ms,
                            &device_rule,
                            device_filter,
                            &payload.drawing_changes,
                            &mut success_ids,
                            &mut upload_status,
                            &mut remote_drawing_changes,
                        )
                        .await?;
                    }
                }

                if uploads_configs {
                    if !payload.configs.is_empty() {
                        process_config_sync_items(
                            &mut tx,
                            &user_uuid,
                            &client_uuid,
                            server_ms,
                            &device_rule,
                            &payload.configs,
                            &mut success_config_uuids,
                            &mut config_broadcasts,
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
                            server_ms,
                            &device_rule,
                            device_filter,
                            &payload.config_changes,
                            &mut success_ids,
                            &mut upload_status,
                            &mut remote_config_changes,
                            &mut config_broadcasts,
                        )
                        .await?;
                    }
                }

                // Fetch remote mutations
                let fetched_config = if scope == SyncScope::ScribbleBox
                    || scope == SyncScope::ScribbleKeep
                    || scope == SyncScope::ScribbleKeepCloud
                {
                    fetch_remote_config_mutations(&mut tx, &user_uuid, &client_uuid, device_filter, payload.last_synced_at).await?
                } else {
                    vec![]
                };

                let fetched_drawing = if scope == SyncScope::ScribbleKeepCloud {
                    fetch_remote_drawing_mutations(&mut tx, &user_uuid, &client_uuid, device_filter, payload.last_synced_at).await?
                } else {
                    vec![]
                };

                // Merge fetched config/drawing changes
                {
                    use std::collections::HashSet;
                    let existing_config_ids: HashSet<String> = remote_config_changes.iter().map(|c| c.id.clone()).collect();
                    remote_config_changes.extend(fetched_config.into_iter().filter(|c| !existing_config_ids.contains(&c.id)));

                    let existing_drawing_ids: HashSet<String> = remote_drawing_changes.iter().map(|c| c.id.clone()).collect();
                    remote_drawing_changes.extend(fetched_drawing.into_iter().filter(|c| !existing_drawing_ids.contains(&c.id)));
                }

                let response_configs = fetch_configs_for_response(
                    &mut tx,
                    &user_uuid,
                    &client_uuid,
                    device_filter,
                    payload.last_synced_at,
                    &success_config_uuids,
                )
                .await?;

                let response_drawings = if scope == SyncScope::ScribbleKeepCloud || (uploads_drawings && !success_drawing_uuids.is_empty()) {
                    fetch_drawings_for_response(
                        &mut tx,
                        &user_uuid,
                        &client_uuid,
                        device_filter,
                        payload.last_synced_at,
                        &success_drawing_uuids,
                        scope == SyncScope::ScribbleKeepCloud,
                    )
                    .await?
                } else {
                    vec![]
                };

                // Only a tablet's own sync counts as the tablet being seen; the cloud
                // app editing a device remotely does not mean that device checked in.
                if let Some(device) = request_device {
                    touch_device(&mut tx, &user_uuid, device).await?;
                }

                tx.commit().await?;

                Ok::<_, AppError>((success_ids, upload_status, remote_config_changes, remote_drawing_changes, response_configs, response_drawings, config_broadcasts))
            } else {
                Ok((vec![], vec![], vec![], vec![], vec![], vec![], vec![]))
            }
        }
    };

    let (todo_res, grocery_res, config_drawing_res) = tokio::try_join!(todo_future, grocery_future, config_drawing_future)?;

    // Consolidate success_ids & upload_status
    let mut success_ids = Vec::new();
    let mut upload_status = Vec::new();

    success_ids.extend(todo_res.0);
    upload_status.extend(todo_res.1);
    let remote_todo_list_changes = todo_res.2;
    let remote_todo_changes = todo_res.3;

    success_ids.extend(grocery_res.0);
    upload_status.extend(grocery_res.1);
    let remote_grocery_list_changes = grocery_res.2;
    let remote_grocery_list_member_changes = grocery_res.3;
    let remote_store_changes = grocery_res.4;
    let remote_category_changes = grocery_res.5;
    let remote_grocery_changes = grocery_res.6;
    let remote_grocery_item_store_info_changes = grocery_res.7;
    let mut affected_grocery_users = grocery_res.8;

    success_ids.extend(config_drawing_res.0);
    upload_status.extend(config_drawing_res.1);
    let remote_config_changes = config_drawing_res.2;
    let remote_drawing_changes = config_drawing_res.3;
    let response_configs = config_drawing_res.4;
    let response_drawings = config_drawing_res.5;
    let config_broadcasts = config_drawing_res.6;

    // Fan each config write out to its own device's Pub/Sub channel, so an SSE stream that
    // named that device sees the change without waiting for its next sync poll. Publishing
    // happens after the transaction commits, so a listener that reacts by syncing reads the
    // row that was just written. A Redis outage must not fail the sync, so failures only log.
    for broadcast in &config_broadcasts {
        let event = SyncSseEvent::DirectUpdate {
            entity: "config".to_string(),
            key: broadcast.item.key.clone(),
            // The value itself, not the whole row: `key`/`value` is the contract every
            // listener reads, and handing it the serialized `ConfigSyncItem` made clients
            // store that JSON object as the config's value.
            value: serde_json::Value::String(broadcast.item.value.clone()),
            sender_client_id: Some(payload.client_id.clone()),
            device_uuid: Some(broadcast.device_uuid),
            is_deleted: broadcast.item.is_deleted,
        };
        if let Err(err) = publish_device_event(
            &state.redis_publisher,
            &claims.sub,
            &broadcast.device_uuid,
            &event,
        )
        .await
        {
            tracing::warn!(
                "Failed to publish config event for device {}: {:?}",
                broadcast.device_uuid,
                err
            );
        }
    }

    let has_grocery = !payload.grocery_list_changes.is_empty()
        || !payload.grocery_list_member_changes.is_empty()
        || !payload.store_changes.is_empty()
        || !payload.category_changes.is_empty()
        || !payload.grocery_changes.is_empty()
        || !payload.grocery_item_store_info_changes.is_empty();

    let has_mutations = !payload.todo_list_changes.is_empty()
        || !payload.todo_changes.is_empty()
        || has_grocery
        || !payload.config_changes.is_empty()
        || !payload.drawing_changes.is_empty()
        || !payload.configs.is_empty()
        || !payload.drawings.is_empty();

    if has_mutations {
        // Connected once and inspected, rather than two `if let`s: the failure needs
        // to be counted, and a second connect attempt just to log it would double the
        // cost of the path that is already failing.
        let cache_conn = state.redis_client.get_multiplexed_tokio_connection().await;
        if let Err(ref err) = cache_conn {
            // Losing this write is not a correctness problem — `/api/sync/status`
            // falls back to the database aggregate — but it is a silent, expensive
            // degradation, so it gets counted.
            crate::observability::metrics::record_redis_degraded("sync_cache_write_connect");
            tracing::warn!("Failed to connect to Redis to update sync caches: {:?}", err);
        }
        if let Ok(mut conn) = cache_conn {
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
            let has_drawings = !payload.drawing_changes.is_empty() || !payload.drawings.is_empty();
            let has_configs = !payload.config_changes.is_empty() || !payload.configs.is_empty();

            if has_todo {
                let _ = conn.set_ex::<_, _, ()>(&format!("user:{}:last_update:Todo", claims.sub), &ts_str, 86400).await;
            }
            // Drawings can now arrive under either the Box or Keep scope, but only the
            // Box and KeepCloud status queries look at drawings, so mark those keys.
            if has_drawings {
                let _ = conn.set_ex::<_, _, ()>(&format!("user:{}:last_update:ScribbleBox", claims.sub), &ts_str, 86400).await;
                let _ = conn.set_ex::<_, _, ()>(&format!("user:{}:last_update:ScribbleKeepCloud", claims.sub), &ts_str, 86400).await;
            }
            if has_configs {
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

    let downloaded = remote_todo_list_changes.len()
        + remote_todo_changes.len()
        + remote_grocery_list_changes.len()
        + remote_grocery_list_member_changes.len()
        + remote_store_changes.len()
        + remote_category_changes.len()
        + remote_grocery_changes.len()
        + remote_grocery_item_store_info_changes.len()
        + remote_config_changes.len()
        + remote_drawing_changes.len()
        + response_configs.len()
        + response_drawings.len();
    crate::observability::http::record_sync_completed(
        &format!("{:?}", scope),
        success_ids.len(),
        downloaded,
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
