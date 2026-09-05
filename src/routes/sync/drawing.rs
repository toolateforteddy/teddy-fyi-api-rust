use crate::routes::sync::device::{ItemDeviceRule, resolve_item_device};
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

