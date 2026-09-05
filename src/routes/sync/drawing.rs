use crate::routes::sync::device::{
    ItemDeviceRule, registered_device_set, resolve_item_device_cached,
};
use crate::routes::sync::deletes::ack_unsynced_delete;
use crate::routes::sync::types::*;
use crate::routes::sync::versioning::{advance_version, seed_version};
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// A `drawings` row with its `data` blob, for the one path that echoes the whole row back.
#[derive(Clone)]
struct CachedDrawing {
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

/// The `SELECT`s a batch of drawing writes would otherwise make one item at a time.
///
/// Every one of them reads the same row under the same predicate — `id`, this account, and
/// the request's `device_filter` — so the whole batch's rows come back in one statement and
/// the loop reads a `HashMap`.
///
/// # The `data` blob is fetched separately, and only for the path that reads it
///
/// Three of the four per-item statements only ever looked at `version`; the fourth, the
/// need-update delta, echoes the entire row including `data`. That blob is bounded at
/// [`crate::routes::sync::limits::DEFAULT_MAX_DRAWING_DATA_BYTES`] — half a megabyte — and a
/// collection may carry ten thousand items, so prefetching it for the whole batch would
/// trade an N+1 for gigabytes of heap. Only the ids that actually ask for it are fetched
/// with it; the rest of the batch gets `id -> version` and nothing else.
///
/// # Staleness
///
/// The loop writes as it goes, and a payload may name the same drawing twice. An id this
/// batch has written is marked stale and re-read with the original per-item statement,
/// which is the only way for the second write to see the version the first one assigned.
struct DrawingBatch {
    user_id: Uuid,
    device_filter: Option<Uuid>,
    versions: HashMap<Uuid, i32>,
    rows: HashMap<Uuid, CachedDrawing>,
    /// What the two prefetches asked about. An id outside them has no cached answer, so a
    /// miss must not be read as "no such row".
    prefetched_versions: HashSet<Uuid>,
    prefetched_rows: HashSet<Uuid>,
    stale: HashSet<Uuid>,
}

impl DrawingBatch {
    async fn load(
        tx: &mut Transaction<'_, Postgres>,
        user_id: &Uuid,
        device_filter: Option<Uuid>,
        ids: &[Uuid],
        blob_ids: &[Uuid],
    ) -> Result<Self, AppError> {
        let mut batch = DrawingBatch {
            user_id: *user_id,
            device_filter,
            versions: HashMap::new(),
            rows: HashMap::new(),
            prefetched_versions: ids.iter().copied().collect(),
            prefetched_rows: blob_ids.iter().copied().collect(),
            stale: HashSet::new(),
        };

        if !ids.is_empty() {
            let rows = sqlx::query!(
                "SELECT id, version FROM drawings \
                 WHERE id = ANY($1) AND user_id = $2 AND ($3::uuid IS NULL OR device_uuid = $3)",
                ids,
                user_id,
                device_filter
            )
            .fetch_all(&mut **tx)
            .await?;
            batch.versions = rows.into_iter().map(|row| (row.id, row.version)).collect();
        }

        if !blob_ids.is_empty() {
            let rows = sqlx::query!(
                "SELECT id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, created_at, data \
                 FROM drawings WHERE id = ANY($1) AND user_id = $2 AND ($3::uuid IS NULL OR device_uuid = $3)",
                blob_ids,
                user_id,
                device_filter
            )
            .fetch_all(&mut **tx)
            .await?;
            for row in rows {
                batch.rows.insert(
                    row.id,
                    CachedDrawing {
                        id: row.id,
                        user_id: row.user_id,
                        device_uuid: row.device_uuid,
                        client_uuid: row.client_uuid,
                        version: row.version,
                        is_deleted: row.is_deleted,
                        last_modified: row.last_modified,
                        sync_state: row.sync_state,
                        created_at: row.created_at,
                        data: row.data,
                    },
                );
            }
        }

        Ok(batch)
    }

    /// The version of the row `id` names, or `None` when there is no such row to write on
    /// top of.
    async fn version_of(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
    ) -> Result<Option<i32>, AppError> {
        if self.prefetched_versions.contains(&id) && !self.stale.contains(&id) {
            return Ok(self.versions.get(&id).copied());
        }

        let row = sqlx::query!(
            "SELECT version FROM drawings \
             WHERE id = $1 AND user_id = $2 AND ($3::uuid IS NULL OR device_uuid = $3)",
            id,
            self.user_id,
            self.device_filter
        )
        .fetch_optional(&mut **tx)
        .await?;

        Ok(row.map(|row| row.version))
    }

    /// The whole row, `data` included, for the need-update path.
    async fn row_for(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        id: Uuid,
    ) -> Result<Option<CachedDrawing>, AppError> {
        if self.prefetched_rows.contains(&id) && !self.stale.contains(&id) {
            return Ok(self.rows.get(&id).cloned());
        }

        let row = sqlx::query!(
            "SELECT id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, created_at, data \
             FROM drawings WHERE id = $1 AND user_id = $2 AND ($3::uuid IS NULL OR device_uuid = $3)",
            id,
            self.user_id,
            self.device_filter
        )
        .fetch_optional(&mut **tx)
        .await?;

        Ok(row.map(|row| CachedDrawing {
            id: row.id,
            user_id: row.user_id,
            device_uuid: row.device_uuid,
            client_uuid: row.client_uuid,
            version: row.version,
            is_deleted: row.is_deleted,
            last_modified: row.last_modified,
            sync_state: row.sync_state,
            created_at: row.created_at,
            data: row.data,
        }))
    }

    /// Marks a row this batch has written, so a later item naming the same drawing reads
    /// what was written rather than what was prefetched.
    fn note_write(&mut self, id: Uuid) {
        self.stale.insert(id);
    }
}

/// Whether a delta is the "tell me what you have" shape: an update that carries no data.
///
/// Factored out so the prefetch pass and the loop cannot disagree about which deltas read
/// the `data` blob — if they did, the prefetch would silently miss and the loop would fall
/// back per item, which is the cost this change exists to remove.
fn delta_is_need_update(change: &DrawingChangeDelta) -> bool {
    matches!(change.operation_type, OperationType::Update)
        && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false))
}

/// The device a change delta names, without deserializing its payload.
///
/// Only used to build the batch's registered-device set, where a `None` costs one fallback
/// statement rather than a wrong answer — which is why peeking at the JSON is good enough.
fn peek_delta_device(explicit: Option<Uuid>, data: Option<&serde_json::Value>) -> Option<Uuid> {
    explicit.or_else(|| {
        data?
            .get("device_uuid")
            .and_then(|v| v.as_str())
            .and_then(|v| Uuid::parse_str(v).ok())
    })
}

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
    // One pass over the batch to work out what the loop will ask for, then one statement
    // each instead of one per item. The `data` blob is only prefetched for the deltas that
    // read it — see `DrawingBatch`.
    let mut ids: Vec<Uuid> = Vec::with_capacity(changes.len());
    let mut blob_ids: Vec<Uuid> = Vec::new();
    let mut devices: HashSet<Uuid> = HashSet::new();
    for change in changes {
        let change_uuid = super::remote_mutations::parse_or_hash_uuid(&change.id);
        ids.push(change_uuid);
        if delta_is_need_update(change) {
            blob_ids.push(change_uuid);
        }
        if let Some(device_uuid) = peek_delta_device(change.device_uuid, change.data.as_ref()) {
            devices.insert(device_uuid);
        }
    }
    let devices: Vec<Uuid> = devices.into_iter().collect();

    let registered = registered_device_set(tx, user_id, device_rule, &devices).await?;
    let mut batch = DrawingBatch::load(tx, user_id, device_filter, &ids, &blob_ids).await?;

    for change in changes {
        let change_id = &change.id;
        let change_uuid = super::remote_mutations::parse_or_hash_uuid(change_id);
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing drawing {}", change_id);

                let is_need_update = delta_is_need_update(change);

                if is_need_update {
                    let existing = batch.row_for(tx, change_uuid).await?;

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
                            let device_uuid = resolve_item_device_cached(
                                tx,
                                user_id,
                                change.device_uuid.or(item.device_uuid),
                                device_rule,
                                "Drawing",
                                change_id,
                                &registered,
                            )
                            .await?;

                            // Fetch existing drawing from the batch prefetch
                            let existing = batch.version_of(tx, change_uuid).await?;

                            // The whole conflict decision, and the only place a version
                            // number comes from: the server's row when there is one, the
                            // bounded client seed when this drawing is new. The client's
                            // `last_modified` is deliberately not consulted — see
                            // `crate::routes::sync::versioning` for the policy and for why
                            // a clock nobody can verify must not decide who wins.
                            let next_version = match existing {
                                Some(version) => {
                                    if item.version < version {
                                        tracing::warn!(
                                            "Conflicting write for drawing {} (client version {}, server version {}); accepting it as the later arrival",
                                            change_id, item.version, version
                                        );
                                    }
                                    advance_version("Drawing", change_id, version)?
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

                            batch.note_write(change_uuid);

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
                    let existing = batch.version_of(tx, change_uuid).await?;

                    if let Some(version) = existing {
                        let next_version = advance_version("Drawing", change_id, version)?;
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

                        batch.note_write(change_uuid);

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
                let existing = batch.version_of(tx, change_uuid).await?;

                if let Some(version) = existing {
                    let next_version = advance_version("Drawing", change_id, version)?;
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

                    batch.note_write(change_uuid);

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

pub async fn fetch_remote_drawing_mutations(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    device_filter: Option<Uuid>,
    last_synced_at: Option<DateTime<Utc>>,
) -> Result<Vec<DrawingChangeDelta>, AppError> {
    let mut remote_changes = Vec::new();
    let is_initial_sync = last_synced_at.is_none() || last_synced_at.map(|t| t.timestamp() <= 0).unwrap_or(true);
    let last_synced_ms = last_synced_at.map(|t| t.timestamp_millis()).unwrap_or(0);

    let rows = sqlx::query!(
        "SELECT id, user_id, device_uuid, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, created_at, data \
         FROM drawings \
         WHERE user_id = $1 AND last_modified > $2 AND ($4 OR client_uuid != $3) AND ($4 = FALSE OR is_deleted = FALSE) \
           AND ($5::uuid IS NULL OR device_uuid = $5)",
        user_id,
        last_synced_ms,
        client_id,
        is_initial_sync,
        device_filter
    )
    .fetch_all(&mut **tx)
    .await?;

    for row in rows {
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
    }

    Ok(remote_changes)
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
) -> Result<(), AppError> {
    // No need-update path on the flat list, so nothing here reads the `data` blob and the
    // prefetch is versions only.
    let ids: Vec<Uuid> = items.iter().map(|item| item.id).collect();
    let devices: Vec<Uuid> = items
        .iter()
        .filter_map(|item| item.device_uuid)
        .collect::<HashSet<Uuid>>()
        .into_iter()
        .collect();

    let registered = registered_device_set(tx, user_id, device_rule, &devices).await?;
    let mut batch = DrawingBatch::load(tx, user_id, device_filter, &ids, &[]).await?;

    for item in items {
        let is_delete = item.is_deleted || item.sync_state == "PENDING_DELETE";

        if is_delete {
            let existing = batch.version_of(tx, item.id).await?;

            if let Some(version) = existing {
                let next_version = advance_version("Drawing", &item.id.to_string(), version)?;
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

                batch.note_write(item.id);
            }
            success_uuids.push(item.id);
        } else {
            let device_uuid = resolve_item_device_cached(
                tx,
                user_id,
                item.device_uuid,
                device_rule,
                "Drawing",
                &item.id.to_string(),
                &registered,
            )
            .await?;

            // Upsert drawing
            let existing = batch.version_of(tx, item.id).await?;

            // Same policy as the change-delta path above: the server's row decides the
            // numbering, the client's clock decides nothing, and a new row's seed is
            // bounded. See `crate::routes::sync::versioning`.
            let next_version = match existing {
                Some(version) => {
                    if item.version < version {
                        tracing::warn!(
                            "Conflicting write for drawing {} (client version {}, server version {}); accepting it as the later arrival",
                            item.id, item.version, version
                        );
                    }
                    advance_version("Drawing", &item.id.to_string(), version)?
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

            batch.note_write(item.id);

            success_uuids.push(item.id);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn fetch_drawings_for_response(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    device_filter: Option<Uuid>,
    last_synced_at: Option<DateTime<Utc>>,
    success_uuids: &[Uuid],
    include_remote_drawings: bool,
) -> Result<Vec<DrawingSyncItem>, AppError> {
    let is_initial_sync = last_synced_at.is_none() || last_synced_at.map(|t| t.timestamp() <= 0).unwrap_or(true);
    let last_synced_ms = last_synced_at.map(|t| t.timestamp_millis()).unwrap_or(0);

    let items = if include_remote_drawings {
        let rows = sqlx::query!(
            "SELECT id, user_id, device_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, created_at, data \
             FROM drawings \
             WHERE user_id = $1 AND ((last_modified > $2 AND ($5 OR client_uuid != $3)) OR id = ANY($4)) \
               AND ($6::uuid IS NULL OR device_uuid = $6)",
            user_id,
            last_synced_ms,
            client_id,
            success_uuids,
            is_initial_sync,
            device_filter
        )
        .fetch_all(&mut **tx)
        .await?;

        rows.into_iter()
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
            .collect()
    } else {
        let rows = sqlx::query!(
            "SELECT id, user_id, device_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, created_at, data \
             FROM drawings \
             WHERE user_id = $1 AND id = ANY($2)",
            user_id,
            success_uuids
        )
        .fetch_all(&mut **tx)
        .await?;

        rows.into_iter()
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
            .collect()
    };

    Ok(items)
}

