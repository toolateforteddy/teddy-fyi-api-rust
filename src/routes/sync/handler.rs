use super::grocery::*;
use super::remote_mutations::*;
use super::todo::*;
use super::types::*;
use super::config::*;
use super::device::*;
use super::limits::{validate_sync_payload, SyncLimits};
use super::scope_auth::authorize_scope;
use super::drawing::*;
use super::publisher::{publish_device_events, SyncSseEvent};
use crate::state::AppState;
use crate::auth::tokens::Claims;
use axum::{
    extract::{Json, State},
    Extension,
};
use chrono::Utc;

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

    // Before the bounds check, and long before a transaction: the scope decides which
    // product's tables this request reaches, and it arrives in the body. `authorize_scope`
    // is what ties it to the product the token was issued for. See
    // `crate::routes::sync::scope_auth`.
    authorize_scope(&claims, scope)?;

    // Bounds first, before a transaction is opened or a row is touched: an over-large
    // drawing blob or an over-long config value fails the whole request with a 400 that
    // names the field, and nothing is written. See `crate::routes::sync::limits`.
    let limits = SyncLimits::from_env();
    validate_sync_payload(&payload, &limits)?;

    // The device this request speaks for is the one in the token, never the one in the body.
    //
    // `client_id` is not an inert label: it becomes `updated_by_client`/`client_uuid` on every
    // row written here, it is the `sender_client_id` on the SSE events published below, and it
    // is what the echo filters compare against when deciding which of an account's devices
    // *skips* a change. `require_auth` has already proved that `X-Client-UUID` equals the
    // token's `client_uuid`, but the body field was never checked against either — so an
    // authenticated caller could name a sibling device of their own account, attribute writes
    // to it, and have the change suppressed on exactly the device it was aimed at. The
    // authenticated caller owns both devices, so this is not a cross-account hole; it is a way
    // to make a tablet miss updates it should have received, silently, from a device the
    // parent may no longer control.
    //
    // Derived rather than rejected. Every client we ship already sends the two identically —
    // the Android `SyncRequestBody` puts the same `clientUuid` in the header and in
    // `client_id` — so a 4xx would buy nothing here and would hard-fail any client that
    // happens to differ, whereas overriding keeps it working and gets the attribution *more*
    // right than what it sent. A disagreement is still worth seeing, so it is logged: if that
    // line never appears in production, the derivation can be tightened into a rejection.
    let mut payload = payload;
    if payload.client_id != claims.client_uuid {
        tracing::warn!(
            token_client_uuid = %claims.client_uuid,
            body_client_id = %payload.client_id,
            "Sync request body named a different client than its token; using the token's."
        );
        payload.client_id = claims.client_uuid.clone();
    }
    let payload = payload;

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
                // Before the transaction, deliberately: this may make an outbound Gemini
                // call, and doing that while holding a connection open inside a live
                // transaction is how a handful of concurrent syncs exhausts the pool and
                // times out unrelated endpoints. See `crate::routes::sync::todo::icons`.
                let resolved_icons =
                    resolve_todo_icons(&state, &claims.sub, &payload.todo_changes).await;

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
                    &resolved_icons,
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
                            &mut upload_status,
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
                            &mut upload_status,
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

                // Fetch remote mutations.
                //
                // One read per entity, feeding both wire channels. `remote_*_changes` and
                // the flat `configs`/`drawings` arrays used to be filled by two queries
                // whose predicates overlapped completely, so every row travelled twice
                // through Postgres, through memory and onto the wire. They are two views
                // of one page of rows now; the wire is unchanged.
                let config_download = if scope == SyncScope::ScribbleBox
                    || scope == SyncScope::ScribbleKeep
                    || scope == SyncScope::ScribbleKeepCloud
                {
                    fetch_config_download(&mut tx, &user_uuid, &client_uuid, device_filter, payload.last_synced_at, limits.download_page_size).await?
                } else {
                    ConfigDownload { remote_changes: vec![], items: vec![], next_cursor_ms: None }
                };

                let drawing_download = if scope == SyncScope::ScribbleKeepCloud {
                    fetch_drawing_download(&mut tx, &user_uuid, &client_uuid, device_filter, payload.last_synced_at, limits.download_page_size).await?
                } else {
                    DrawingDownload { remote_changes: vec![], items: vec![], next_cursor_ms: None }
                };

                // Merge fetched config/drawing changes
                {
                    use std::collections::HashSet;
                    let existing_config_ids: HashSet<String> = remote_config_changes.iter().map(|c| c.id.clone()).collect();
                    remote_config_changes.extend(config_download.remote_changes.into_iter().filter(|c| !existing_config_ids.contains(&c.id)));

                    let existing_drawing_ids: HashSet<String> = remote_drawing_changes.iter().map(|c| c.id.clone()).collect();
                    remote_drawing_changes.extend(drawing_download.remote_changes.into_iter().filter(|c| !existing_drawing_ids.contains(&c.id)));
                }

                // The upload echo is read separately and merged, rather than folded into
                // the download's predicate as it used to be: the download is paged now,
                // and a page limit must never be able to swallow the acknowledgement for
                // a row the client is holding open.
                let mut response_configs = config_download.items;
                {
                    use std::collections::HashSet;
                    let already: HashSet<uuid::Uuid> = response_configs.iter().map(|c| c.id).collect();
                    let echoed = fetch_configs_for_echo(&mut tx, &user_uuid, device_filter, &success_config_uuids).await?;
                    response_configs.extend(echoed.into_iter().filter(|c| !already.contains(&c.id)));
                }

                // The cloud scope uploads no drawings and the tablet scopes download
                // none, so exactly one of these two is ever non-empty — the overlap the
                // old single query had to reconcile does not exist here.
                let mut response_drawings = drawing_download.items;
                if uploads_drawings && !success_drawing_uuids.is_empty() {
                    use std::collections::HashSet;
                    let already: HashSet<uuid::Uuid> = response_drawings.iter().map(|d| d.id).collect();
                    let echoed = fetch_drawings_for_response(&mut tx, &user_uuid, &success_drawing_uuids).await?;
                    response_drawings.extend(echoed.into_iter().filter(|d| !already.contains(&d.id)));
                }

                // A truncated page must not let the client's cursor jump to this
                // request's `server_timestamp`, or everything left behind becomes
                // unreachable. The smallest truncation point wins, because one
                // `server_timestamp` serves every entity in the reply.
                let next_cursor_ms = [config_download.next_cursor_ms, drawing_download.next_cursor_ms]
                    .into_iter()
                    .flatten()
                    .min();

                // Only a tablet's own sync counts as the tablet being seen; the cloud
                // app editing a device remotely does not mean that device checked in.
                if let Some(device) = request_device {
                    touch_device(&mut tx, &user_uuid, device).await?;
                }

                tx.commit().await?;

                Ok::<_, AppError>((success_ids, upload_status, remote_config_changes, remote_drawing_changes, response_configs, response_drawings, config_broadcasts, next_cursor_ms))
            } else {
                Ok((vec![], vec![], vec![], vec![], vec![], vec![], vec![], None))
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
    let next_cursor_ms = config_drawing_res.7;

    // Fan each config write out to its own device's Pub/Sub channel, so an SSE stream that
    // named that device sees the change without waiting for its next sync poll. Publishing
    // happens after the transaction commits, so a listener that reacts by syncing reads the
    // row that was just written. A Redis outage must not fail the sync, so failures only log.
    //
    // The whole set goes out as one pipelined batch rather than a publish apiece: a payload
    // of 500 configs is 500 broadcasts, and sending them serially put 500 in-request round
    // trips on the tail of a sync that had already done its real work. They are independent
    // of one another, so there was never anything to gain from waiting for each reply.
    let config_events: Vec<(uuid::Uuid, SyncSseEvent)> = config_broadcasts
        .iter()
        .map(|broadcast| {
            (
                broadcast.device_uuid,
                SyncSseEvent::DirectUpdate {
                    entity: "config".to_string(),
                    key: broadcast.item.key.clone(),
                    // The value itself, not the whole row: `key`/`value` is the contract every
                    // listener reads, and handing it the serialized `ConfigSyncItem` made clients
                    // store that JSON object as the config's value.
                    value: serde_json::Value::String(broadcast.item.value.clone()),
                    sender_client_id: Some(payload.client_id.clone()),
                    device_uuid: Some(broadcast.device_uuid),
                    is_deleted: broadcast.item.is_deleted,
                },
            )
        })
        .collect();
    if let Err(err) =
        publish_device_events(&state.redis_publisher, &claims.sub, &config_events).await
    {
        tracing::warn!(
            "Failed to publish {} config event(s): {:?}",
            config_events.len(),
            err
        );
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
        // Every key this request touches goes out in one pipeline, on the process-wide
        // connection `RedisPublisher` already holds. Both halves of that used to be a
        // per-request cost: a fresh multiplexed connection (a TCP, and in production a
        // TLS, handshake) dialled and thrown away on every mutating sync, and then a
        // serial `SET` per key — two of them per member of every shared grocery list
        // touched, so a four-member list alone was eight round trips.
        let ts_str = server_timestamp.to_rfc3339();
        let mut cache_writes = redis::pipe();

        // Update All scope for the requesting user
        cache_writes.set_ex(format!("user:{}:last_update:All", claims.sub), &ts_str, 86400).ignore();

        // Invalidate/update caches for all members/collaborators of the updated grocery lists
        if has_grocery {
            if !affected_grocery_users.contains(&claims.sub) {
                affected_grocery_users.push(claims.sub.clone());
            }
            for user_id in &affected_grocery_users {
                cache_writes.set_ex(format!("user:{}:last_update:Grocery", user_id), &ts_str, 86400).ignore();
                cache_writes.set_ex(format!("user:{}:last_update:All", user_id), &ts_str, 86400).ignore();
            }
        }

        // Update specific scopes for the requesting user
        let has_todo = !payload.todo_list_changes.is_empty() || !payload.todo_changes.is_empty();
        let has_drawings = !payload.drawing_changes.is_empty() || !payload.drawings.is_empty();
        let has_configs = !payload.config_changes.is_empty() || !payload.configs.is_empty();

        if has_todo {
            cache_writes.set_ex(format!("user:{}:last_update:Todo", claims.sub), &ts_str, 86400).ignore();
        }
        // Drawings can now arrive under either the Box or Keep scope, but only the
        // Box and KeepCloud status queries look at drawings, so mark those keys.
        if has_drawings {
            cache_writes.set_ex(format!("user:{}:last_update:ScribbleBox", claims.sub), &ts_str, 86400).ignore();
            cache_writes.set_ex(format!("user:{}:last_update:ScribbleKeepCloud", claims.sub), &ts_str, 86400).ignore();
        }
        if has_configs {
            cache_writes.set_ex(format!("user:{}:last_update:ScribbleKeep", claims.sub), &ts_str, 86400).ignore();
            cache_writes.set_ex(format!("user:{}:last_update:ScribbleKeepCloud", claims.sub), &ts_str, 86400).ignore();
        }

        if let Err(err) = state.redis_publisher.run_pipeline(&cache_writes).await {
            // Losing this write is not a correctness problem — `/api/sync/status`
            // falls back to the database aggregate — but it is a silent, expensive
            // degradation, so it gets counted. The counter keeps its name, and still
            // means "this request's cache keys did not get written"; what changed is
            // that there is no separate connect step to attribute a failure to, since
            // the dial is shared and lazy, and that the batch shares one outcome
            // instead of each key failing on its own.
            crate::observability::metrics::record_redis_degraded("sync_cache_write_connect");
            tracing::warn!("Failed to update sync caches in Redis: {:?}", err);
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

    // The cursor the client is handed. Normally this request's clock reading; when a
    // download was paged, the last millisecond this reply delivered whole instead — the
    // client's cursor may not move past rows it did not receive. Every entity in the
    // reply shares one `server_timestamp`, so an unpaged one (todo, grocery) is simply
    // re-read on the next sync, which its own echo-suppression filter makes cheap and
    // which the sync protocol is idempotent under regardless.
    let has_more = next_cursor_ms.is_some();
    let server_timestamp = match next_cursor_ms {
        Some(ms) => chrono::DateTime::from_timestamp_millis(ms).unwrap_or(server_timestamp),
        None => server_timestamp,
    };

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
        has_more,
    }))
}
