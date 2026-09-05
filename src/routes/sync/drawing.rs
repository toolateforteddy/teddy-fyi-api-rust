use crate::routes::sync::device::{ItemDeviceRule, resolve_item_device};
use crate::routes::sync::deletes::ack_unsynced_delete;
use crate::routes::sync::paging::{trim_page, Page};
use crate::routes::sync::types::*;
use crate::routes::sync::versioning::{advance_version, seed_version};
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

#[allow(clippy::too_many_arguments)]
pub async fn process_drawing_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    // This request's server clock reading, in epoch milliseconds, stamped onto every row
    // written here. It is both the ordering the conflict policy is defined in and the
    // cursor the next download compares against, so it has to come from the server and
    // has to be the same instant the response reports as `server_timestamp`.
    server_ms: i64,
    device_rule: &ItemDeviceRule,
    device_filter: Option<Uuid>,
    changes: &[DrawingChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
    remote_changes: &mut Vec<DrawingChangeDelta>,
) -> Result<(), AppError> {
    for change in changes {
        let change_id = &change.id;
        let change_uuid = super::remote_mutations::parse_or_hash_uuid(change_id);
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing drawing {}", change_id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    let existing = sqlx::query!(
                        "SELECT id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, created_at, data \
                         FROM drawings WHERE id = $1 AND user_id = $2 AND ($3::uuid IS NULL OR device_uuid = $3)",
                        change_uuid,
                        user_id,
                        device_filter
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = existing {
                        let item_data = DrawingData {
                            id: row.id,
                            user_id: row.user_id.to_string(),
                            client_uuid: row.client_uuid.to_string(),
                            device_uuid: Some(row.device_uuid),
                            version: row.version,
                            is_deleted: row.is_deleted,
                            last_modified: row.last_modified,
                            sync_state: row.sync_state.clone().unwrap_or_else(|| "SYNCED".to_string()),
                            created_at: row.created_at,
                            data: row.data,
                        };
                        let data_val = serde_json::to_value(&item_data)?;
                        remote_changes.push(DrawingChangeDelta {
                            id: change_id.to_string(),
                            operation_type: OperationType::Update,
                            version: row.version,
                            device_uuid: Some(row.device_uuid),
                            data: Some(data_val),
                        });
                        success_ids.push(change_id.to_string());
                    }
                    continue;
                }

                if let Some(ref data) = change.data {
                    match serde_json::from_value::<DrawingData>(data.clone()) {
                        Ok(item) => {
                            let device_uuid = resolve_item_device(
                                tx,
                                user_id,
                                change.device_uuid.or(item.device_uuid),
                                device_rule,
                                "Drawing",
                                change_id,
                            )
                            .await?;

                            // Fetch existing drawing from database
                            let existing = sqlx::query!(
                                "SELECT version FROM drawings \
                                 WHERE id = $1 AND user_id = $2 AND ($3::uuid IS NULL OR device_uuid = $3)",
                                change_uuid,
                                user_id,
                                device_filter
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            // The whole conflict decision, and the only place a version
                            // number comes from: the server's row when there is one, the
                            // bounded client seed when this drawing is new. The client's
                            // `last_modified` is deliberately not consulted — see
                            // `crate::routes::sync::versioning` for the policy and for why
                            // a clock nobody can verify must not decide who wins.
                            let next_version = match existing {
                                Some(ref row) => {
                                    if item.version < row.version {
                                        tracing::warn!(
                                            "Conflicting write for drawing {} (client version {}, server version {}); accepting it as the later arrival",
                                            change_id, item.version, row.version
                                        );
                                    }
                                    advance_version("Drawing", change_id, row.version)?
                                }
                                None => seed_version("Drawing", change_id, item.version)?,
                            };

                            tracing::info!(
                                "Applying drawing upsert for {}. Version: {}, is_deleted: {}",
                                change_id,
                                next_version,
                                item.is_deleted
                            );

                            // Drawing ids are hashed from client strings when they are not UUIDs, so the same
                            // id can arrive from two different accounts. Without the guard on the conflict
                            // target, one account's upload would overwrite the other's row.
                            sqlx::query!(
                                "INSERT INTO drawings (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, client_last_modified, sync_state, created_at, data) \
                                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::text::sync_state, $10, $11) \
                                 ON CONFLICT (id) DO UPDATE SET \
                                     device_uuid = EXCLUDED.device_uuid, \
                                     client_uuid = EXCLUDED.client_uuid, \
                                     version = EXCLUDED.version, \
                                     is_deleted = EXCLUDED.is_deleted, \
                                     last_modified = EXCLUDED.last_modified, \
                                     client_last_modified = EXCLUDED.client_last_modified, \
                                     sync_state = EXCLUDED.sync_state, \
                                     data = EXCLUDED.data \
                                 WHERE drawings.user_id = EXCLUDED.user_id",
                                change_uuid,
                                user_id,
                                device_uuid,
                                client_id,
                                next_version,
                                item.is_deleted,
                                server_ms,
                                item.last_modified,
                                "SYNCED",
                                item.created_at,
                                item.data
                            )
                            .execute(&mut **tx)
                            .await?;

                            upload_status.push(SuccessResult {
                                id: change_id.to_string(),
                                version: next_version,
                                sync_state: "SYNCED".to_string(),
                            });
                            success_ids.push(change_id.to_string());
                        }
                        Err(err) => {
                            tracing::error!("Failed to deserialize DrawingData for drawing {}: {:?}. Data: {:?}", change_id, err, data);
                            return Err(AppError::Serialization(err));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let existing = sqlx::query!(
                        "SELECT version FROM drawings WHERE id = $1 AND user_id = $2 AND ($3::uuid IS NULL OR device_uuid = $3)",
                        change_uuid,
                        user_id,
                        device_filter
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = existing {
                        let next_version = advance_version("Drawing", change_id, row.version)?;
                        tracing::info!("Applying drawing metadata update for {}. Next version: {}", change_id, next_version);
                        // `last_modified` moves with the write, here as everywhere else:
                        // it is the cursor the account's other devices poll against, so a
                        // row that changes without it changing is a change they never see.
                        sqlx::query!(
                            "UPDATE drawings SET version = $1, client_uuid = $2, last_modified = $3, sync_state = 'SYNCED' WHERE id = $4 AND user_id = $5",
                            next_version,
                            client_id,
                            server_ms,
                            change_uuid,
                            user_id
                        )
                        .execute(&mut **tx)
                        .await?;

                        upload_status.push(SuccessResult {
                            id: change_id.to_string(),
                            version: next_version,
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(change_id.to_string());
                    }
                }
            }
            OperationType::Delete => {
                let existing = sqlx::query!(
                    "SELECT version FROM drawings WHERE id = $1 AND user_id = $2 AND ($3::uuid IS NULL OR device_uuid = $3)",
                    change_uuid,
                    user_id,
                    device_filter
                )
                .fetch_optional(&mut **tx)
                .await?;

                if let Some(row) = existing {
                    let next_version = advance_version("Drawing", change_id, row.version)?;
                    tracing::info!("Applying drawing soft-delete for {}. Next version: {}", change_id, next_version);
                    // Stamped for the same reason as the update above: a soft-delete the
                    // cursor cannot see is a deletion the sibling tablet never applies.
                    sqlx::query!(
                        "UPDATE drawings SET is_deleted = TRUE, version = $1, client_uuid = $2, last_modified = $3, sync_state = 'PENDING_DELETE' WHERE id = $4 AND user_id = $5",
                        next_version,
                        client_id,
                        server_ms,
                        change_uuid,
                        user_id
                    )
                    .execute(&mut **tx)
                    .await?;

                    upload_status.push(SuccessResult {
                        id: change_id.to_string(),
                        version: next_version,
                        sync_state: "SYNCED".to_string(),
                    });
                    success_ids.push(change_id.to_string());
                } else {
                    // Nothing to delete, so the delete has succeeded. Acknowledged rather
                    // than dropped: a change the response never mentions stays pending on
                    // the device and comes back on every sync forever. See
                    // `crate::routes::sync::deletes`.
                    upload_status.push(SuccessResult {
                        id: change_id.to_string(),
                        version: ack_unsynced_delete("drawing", change_id),
                        sync_state: "SYNCED".to_string(),
                    });
                    success_ids.push(change_id.to_string());
                }
            }
        }
    }
    Ok(())
}

/// One page of the drawings a client is owed, read from Postgres exactly once.
///
/// `remote_changes` and `items` are two views of the *same* rows. They used to be two
/// queries — `fetch_remote_drawing_mutations` and the cloud branch of
/// `fetch_drawings_for_response` — whose predicates were one a superset of the other, so
/// every drawing on a cloud sync was read from Postgres twice, held in memory twice and
/// serialized into the reply twice: a straight 2x on the heaviest payload in the service.
///
/// Both fields still go on the wire, unchanged, because there is no artifact anywhere in
/// this repo that says which of `remote_drawing_changes` and `drawings` the shipped
/// clients actually read (`context/2026-09-05_pre_split_changes.md` item 33 is the
/// missing wire contract). Dropping either is a client-visible break and belongs with
/// that contract; halving the database and memory cost of producing them does not, and is
/// what this does.
pub struct DrawingDownload {
    pub remote_changes: Vec<DrawingChangeDelta>,
    pub items: Vec<DrawingSyncItem>,
    /// `Some(ms)` when the page was cut short: the client must resume from this
    /// millisecond rather than from the request's `server_timestamp`. See
    /// `crate::routes::sync::paging`.
    pub next_cursor_ms: Option<i64>,
}

/// Reads at most one page of the drawings changed since the client's cursor.
///
/// Rows come back oldest-first so that the page has a well-defined edge to stop at; the
/// download queries were previously unordered because they were unbounded and order did
/// not matter.
pub async fn fetch_drawing_download(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    device_filter: Option<Uuid>,
    last_synced_at: Option<DateTime<Utc>>,
    page_size: usize,
) -> Result<DrawingDownload, AppError> {
    let is_initial_sync = last_synced_at.is_none() || last_synced_at.map(|t| t.timestamp() <= 0).unwrap_or(true);
    let last_synced_ms = last_synced_at.map(|t| t.timestamp_millis()).unwrap_or(0);

    // One row over the page is the probe that says whether anything is left behind, which
    // is cheaper than a second COUNT over the same predicate.
    let probe_limit = page_size.saturating_add(1) as i64;
    let mut rows = fetch_drawing_page(tx, user_id, client_id, device_filter, last_synced_ms, None, is_initial_sync, probe_limit).await?;

    let next_cursor_ms = match trim_page(&mut rows, page_size, |row| row.last_modified) {
        Page::Complete => None,
        Page::Truncated { next_cursor_ms } => Some(next_cursor_ms),
        Page::WholeMillisecond { ms } => {
            // One request wrote more than a page of drawings, so they all carry its one
            // clock reading and no cursor can split them. Serving the group whole is the
            // only way the client ever gets past it; it is bounded by
            // `DEFAULT_MAX_ITEMS_PER_COLLECTION`, which is what makes that safe.
            tracing::warn!(
                "More than a page of drawings for user {} share last_modified {}; serving that millisecond whole",
                user_id, ms
            );
            rows = fetch_drawing_page(tx, user_id, client_id, device_filter, ms - 1, Some(ms), is_initial_sync, i64::MAX).await?;
            Some(ms)
        }
    };

    let mut remote_changes = Vec::with_capacity(rows.len());
    let mut items = Vec::with_capacity(rows.len());

    for row in rows {
        let sync_state = row.sync_state.unwrap_or_else(|| "SYNCED".to_string());

        // `drawings` has always carried soft-deleted rows on an initial sync while
        // `remote_drawing_changes` has always suppressed them. That asymmetry is on the
        // wire, so it is reproduced here rather than unified — the two channels differ
        // only in this one respect, and only on the first sync.
        let data = if is_initial_sync && row.is_deleted {
            row.data
        } else {
            // The blob is moved into `DrawingData`, serialized, and moved back out. That
            // is one deep copy of it per drawing rather than the two the old pair of
            // queries produced, and it is the floor while both channels carry the same
            // drawing: `serde_json::to_value` has to build a `Value` tree, and the item
            // needs a blob of its own afterwards. Dropping a channel is the only thing
            // that removes the copy, and that is a wire change.
            let mut item_data = DrawingData {
                id: row.id,
                user_id: row.user_id.to_string(),
                client_uuid: row.client_uuid.to_string(),
                device_uuid: Some(row.device_uuid),
                version: row.version,
                is_deleted: row.is_deleted,
                last_modified: row.last_modified,
                sync_state: sync_state.clone(),
                created_at: row.created_at,
                data: row.data,
            };
            let data_val = serde_json::to_value(&item_data)?;

            remote_changes.push(DrawingChangeDelta {
                id: row.id.to_string(),
                operation_type: if row.is_deleted {
                    OperationType::Delete
                } else {
                    OperationType::Update
                },
                version: row.version,
                device_uuid: Some(row.device_uuid),
                data: Some(data_val),
            });

            std::mem::take(&mut item_data.data)
        };

        items.push(DrawingSyncItem {
            id: row.id,
            user_id: Some(row.user_id.to_string()),
            device_uuid: Some(row.device_uuid),
            created_at: row.created_at,
            data,
            sync_state,
            version: row.version,
            is_deleted: row.is_deleted,
            last_modified: row.last_modified,
        });
    }

    Ok(DrawingDownload { remote_changes, items, next_cursor_ms })
}

/// The one download query, shared by the page read and the whole-millisecond re-read.
#[allow(clippy::too_many_arguments)]
async fn fetch_drawing_page(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    device_filter: Option<Uuid>,
    after_ms: i64,
    through_ms: Option<i64>,
    is_initial_sync: bool,
    limit: i64,
) -> Result<Vec<DrawingRow>, AppError> {
    let rows = sqlx::query_as!(
        DrawingRow,
        "SELECT id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, created_at, data \
         FROM drawings \
         WHERE user_id = $1 AND last_modified > $2 AND ($4 OR client_uuid != $3) \
           AND ($5::uuid IS NULL OR device_uuid = $5) \
           AND ($6::bigint IS NULL OR last_modified <= $6) \
         ORDER BY last_modified ASC, id ASC \
         LIMIT $7",
        user_id,
        after_ms,
        client_id,
        is_initial_sync,
        device_filter,
        through_ms,
        limit
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows)
}

struct DrawingRow {
    id: Uuid,
    user_id: Uuid,
    device_uuid: Uuid,
    client_uuid: Uuid,
    version: i32,
    is_deleted: bool,
    last_modified: i64,
    sync_state: Option<String>,
    created_at: i64,
    data: serde_json::Value,
}

#[allow(clippy::too_many_arguments)]
pub async fn process_drawing_sync_items(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    // See `process_drawing_changes`: the server's stamp for every row this call writes.
    server_ms: i64,
    device_rule: &ItemDeviceRule,
    device_filter: Option<Uuid>,
    items: &[DrawingSyncItem],
    success_uuids: &mut Vec<Uuid>,
    // Populated for the same reason the change-delta path populates it: `upload_status`
    // is the documented channel for "processed, and here is the version the server gave
    // it", and the flat `drawings[]` path was the one upload path that reported nothing
    // there. Until it does, the echoed `drawings[]` rows are the only place a client can
    // read the new version back, which is what keeps the blob in that echo.
    upload_status: &mut Vec<SuccessResult>,
) -> Result<(), AppError> {
    for item in items {
        let is_delete = item.is_deleted || item.sync_state == "PENDING_DELETE";

        if is_delete {
            let existing = sqlx::query!(
                "SELECT version FROM drawings WHERE id = $1 AND user_id = $2 AND ($3::uuid IS NULL OR device_uuid = $3)",
                item.id,
                user_id,
                device_filter
            )
            .fetch_optional(&mut **tx)
            .await?;

            if let Some(row) = existing {
                let next_version = advance_version("Drawing", &item.id.to_string(), row.version)?;
                tracing::info!("Applying drawing soft-delete for {}. Next version: {}", item.id, next_version);
                sqlx::query!(
                    "UPDATE drawings SET is_deleted = TRUE, version = $1, client_uuid = $2, last_modified = $3, sync_state = 'PENDING_DELETE'::text::sync_state WHERE id = $4 AND user_id = $5",
                    next_version,
                    client_id,
                    server_ms,
                    item.id,
                    user_id
                )
                .execute(&mut **tx)
                .await?;

                upload_status.push(SuccessResult {
                    id: item.id.to_string(),
                    version: next_version,
                    sync_state: "SYNCED".to_string(),
                });
            } else {
                // Nothing to delete, so the delete has succeeded — acknowledged rather
                // than dropped, exactly as in `process_drawing_changes`. See
                // `crate::routes::sync::deletes`.
                upload_status.push(SuccessResult {
                    id: item.id.to_string(),
                    version: ack_unsynced_delete("drawing", &item.id.to_string()),
                    sync_state: "SYNCED".to_string(),
                });
            }
            success_uuids.push(item.id);
        } else {
            let device_uuid = resolve_item_device(
                tx,
                user_id,
                item.device_uuid,
                device_rule,
                "Drawing",
                &item.id.to_string(),
            )
            .await?;

            // Upsert drawing
            let existing = sqlx::query!(
                "SELECT version FROM drawings \
                 WHERE id = $1 AND user_id = $2 AND ($3::uuid IS NULL OR device_uuid = $3)",
                item.id,
                user_id,
                device_filter
            )
            .fetch_optional(&mut **tx)
            .await?;

            // Same policy as the change-delta path above: the server's row decides the
            // numbering, the client's clock decides nothing, and a new row's seed is
            // bounded. See `crate::routes::sync::versioning`.
            let next_version = match existing {
                Some(ref row) => {
                    if item.version < row.version {
                        tracing::warn!(
                            "Conflicting write for drawing {} (client version {}, server version {}); accepting it as the later arrival",
                            item.id, item.version, row.version
                        );
                    }
                    advance_version("Drawing", &item.id.to_string(), row.version)?
                }
                None => seed_version("Drawing", &item.id.to_string(), item.version)?,
            };

            tracing::info!(
                "Applying drawing upsert for {}. Next version: {}, is_deleted: {}",
                item.id,
                next_version,
                item.is_deleted
            );

            // Drawing ids are hashed from client strings when they are not UUIDs, so the same
            // id can arrive from two different accounts. Without the guard on the conflict
            // target, one account's upload would overwrite the other's row.
            sqlx::query!(
                "INSERT INTO drawings (id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, client_last_modified, sync_state, created_at, data) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9::text::sync_state, $10, $11) \
                 ON CONFLICT (id) DO UPDATE SET \
                     device_uuid = EXCLUDED.device_uuid, \
                     client_uuid = EXCLUDED.client_uuid, \
                     version = EXCLUDED.version, \
                     is_deleted = EXCLUDED.is_deleted, \
                     last_modified = EXCLUDED.last_modified, \
                     client_last_modified = EXCLUDED.client_last_modified, \
                     sync_state = EXCLUDED.sync_state, \
                     data = EXCLUDED.data \
                 WHERE drawings.user_id = EXCLUDED.user_id",
                item.id,
                user_id,
                device_uuid,
                client_id,
                next_version,
                item.is_deleted,
                server_ms,
                item.last_modified,
                "SYNCED",
                item.created_at,
                item.data
            )
            .execute(&mut **tx)
            .await?;

            upload_status.push(SuccessResult {
                id: item.id.to_string(),
                version: next_version,
                sync_state: "SYNCED".to_string(),
            });
            success_uuids.push(item.id);
        }
    }
    Ok(())
}

/// The upload echo: the rows this very request just wrote, read back for the client.
///
/// Only the tablet scopes reach this. The cloud scope uploads no drawings, so its reply
/// comes wholly from [`fetch_drawing_download`] and the two no longer overlap at all —
/// which is the other half of the double-send this change removes.
///
/// It still carries `data`, i.e. the request body handed back out as the response body,
/// and that is the one saving deliberately left on the table here. `DrawingSyncItem.data`
/// is a required field, so a metadata-only echo is a wire break, and until this change
/// the echo was also the *only* place a flat `drawings[]` upload could read its
/// server-assigned version back — `process_drawing_sync_items` reported nothing in
/// `upload_status`. It does now, which is the precondition for dropping the echo (or its
/// blob) in a follow-up that is allowed to change the wire. This one is not.
pub async fn fetch_drawings_for_response(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    success_uuids: &[Uuid],
) -> Result<Vec<DrawingSyncItem>, AppError> {
    let rows = sqlx::query!(
        "SELECT id, user_id, device_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, created_at, data \
         FROM drawings \
         WHERE user_id = $1 AND id = ANY($2)",
        user_id,
        success_uuids
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows
        .into_iter()
        .map(|row| DrawingSyncItem {
            id: row.id,
            user_id: Some(row.user_id.to_string()),
            device_uuid: Some(row.device_uuid),
            created_at: row.created_at,
            data: row.data,
            sync_state: row.sync_state.unwrap_or_else(|| "SYNCED".to_string()),
            version: row.version,
            is_deleted: row.is_deleted,
            last_modified: row.last_modified,
        })
        .collect())
}
