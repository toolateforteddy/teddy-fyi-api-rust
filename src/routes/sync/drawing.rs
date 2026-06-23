use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub async fn process_drawing_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    changes: &[DrawingChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
) -> Result<(), AppError> {
    for change in changes {
        let change_id = &change.id;
        let change_uuid = super::remote_mutations::parse_or_hash_uuid(change_id);
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing drawing {}", change_id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<DrawingData>(data.clone()) {
                        Ok(item) => {
                            // Fetch existing drawing from database
                            let existing = sqlx::query!(
                                "SELECT version, last_modified FROM drawings WHERE id = $1 AND user_id = $2",
                                change_uuid,
                                user_id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            let next_version = if let Some(ref row) = existing {
                                if item.version == row.version {
                                    row.version + 1
                                } else if item.version < row.version {
                                    if item.last_modified >= row.last_modified {
                                        tracing::warn!(
                                            "MVCC Conflict for drawing {}. Client version: {}, Server version: {}. Resolving via LWW (Client wins: client last_modified {} >= server last_modified {}). Overwriting server state.",
                                            change_id, item.version, row.version, item.last_modified, row.last_modified
                                        );
                                        row.version + 1
                                    } else {
                                        tracing::warn!(
                                            "MVCC Conflict for drawing {}. Client version: {}, Server version: {}. Resolving via LWW (Server wins: client last_modified {} < server last_modified {}). Rejecting client update.",
                                            change_id, item.version, row.version, item.last_modified, row.last_modified
                                        );
                                        // Server has a newer write. Reject incoming update, return current server state version
                                        upload_status.push(SuccessResult {
                                            id: change_id.to_string(),
                                            version: row.version,
                                            sync_state: "SYNCED".to_string(),
                                        });
                                        success_ids.push(change_id.to_string());
                                        continue;
                                    }
                                } else {
                                    item.version + 1
                                }
                            } else {
                                item.version
                            };

                            tracing::info!(
                                "Applying drawing upsert for {}. Version: {}, is_deleted: {}",
                                change_id,
                                next_version,
                                item.is_deleted
                            );

                            sqlx::query!(
                                "INSERT INTO drawings (id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data) \
                                 VALUES ($1, $2, $3, $4, $5, $6, $7::text::sync_state, $8, $9) \
                                 ON CONFLICT (id) DO UPDATE SET \
                                     client_uuid = EXCLUDED.client_uuid, \
                                     version = EXCLUDED.version, \
                                     is_deleted = EXCLUDED.is_deleted, \
                                     last_modified = EXCLUDED.last_modified, \
                                     sync_state = EXCLUDED.sync_state, \
                                     data = EXCLUDED.data",
                                change_uuid,
                                user_id,
                                client_id,
                                next_version,
                                item.is_deleted,
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
                            tracing::error!("Failed to deserialize DrawingData for drawing {}: {:?}", change_id, err);
                            return Err(AppError::Serialization(err));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let existing = sqlx::query!(
                        "SELECT version FROM drawings WHERE id = $1 AND user_id = $2",
                        change_uuid,
                        user_id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = existing {
                        let next_version = row.version + 1;
                        tracing::info!("Applying drawing metadata update for {}. Next version: {}", change_id, next_version);
                        sqlx::query!(
                            "UPDATE drawings SET version = $1, client_uuid = $2, sync_state = 'SYNCED' WHERE id = $3 AND user_id = $4",
                            next_version,
                            client_id,
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
                    "SELECT version FROM drawings WHERE id = $1 AND user_id = $2",
                    change_uuid,
                    user_id
                )
                .fetch_optional(&mut **tx)
                .await?;

                if let Some(row) = existing {
                    let next_version = row.version + 1;
                    tracing::info!("Applying drawing soft-delete for {}. Next version: {}", change_id, next_version);
                    sqlx::query!(
                        "UPDATE drawings SET is_deleted = TRUE, version = $1, client_uuid = $2, sync_state = 'PENDING_DELETE' WHERE id = $3 AND user_id = $4",
                        next_version,
                        client_id,
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
    last_synced_at: Option<DateTime<Utc>>,
) -> Result<Vec<DrawingChangeDelta>, AppError> {
    let mut remote_changes = Vec::new();
    let last_synced_ms = last_synced_at.map(|t| t.timestamp_millis()).unwrap_or(0);

    let rows = sqlx::query!(
        "SELECT id, user_id, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, created_at, data \
         FROM drawings \
         WHERE user_id = $1 AND last_modified > $2 AND client_uuid != $3",
        user_id,
        last_synced_ms,
        client_id
    )
    .fetch_all(&mut **tx)
    .await?;

    for row in rows {
        let item_data = DrawingData {
            id: row.id,
            user_id: row.user_id,
            client_uuid: row.client_uuid,
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
            data: Some(data_val),
        });
    }

    Ok(remote_changes)
}

pub async fn process_drawing_sync_items(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    items: &[DrawingSyncItem],
    success_uuids: &mut Vec<Uuid>,
) -> Result<(), AppError> {
    for item in items {
        let is_delete = item.is_deleted || item.sync_state == "PENDING_DELETE";

        if is_delete {
            let existing = sqlx::query!(
                "SELECT version FROM drawings WHERE id = $1 AND user_id = $2",
                item.id,
                user_id
            )
            .fetch_optional(&mut **tx)
            .await?;

            if let Some(row) = existing {
                let next_version = row.version + 1;
                tracing::info!("Applying drawing soft-delete for {}. Next version: {}", item.id, next_version);
                sqlx::query!(
                    "UPDATE drawings SET is_deleted = TRUE, version = $1, client_uuid = $2, sync_state = 'PENDING_DELETE'::text::sync_state WHERE id = $3 AND user_id = $4",
                    next_version,
                    client_id,
                    item.id,
                    user_id
                )
                .execute(&mut **tx)
                .await?;
            }
            success_uuids.push(item.id);
        } else {
            // Upsert drawing
            let existing = sqlx::query!(
                "SELECT version, last_modified FROM drawings WHERE id = $1 AND user_id = $2",
                item.id,
                user_id
            )
            .fetch_optional(&mut **tx)
            .await?;

            let next_version = if let Some(ref row) = existing {
                if item.version == row.version {
                    row.version + 1
                } else if item.version < row.version {
                    if item.last_modified >= row.last_modified {
                        tracing::warn!(
                            "MVCC Conflict for drawing {}. Client version: {}, Server version: {}. Resolving via LWW (Client wins: client last_modified {} >= server last_modified {}). Overwriting server state.",
                            item.id, item.version, row.version, item.last_modified, row.last_modified
                        );
                        row.version + 1
                    } else {
                        tracing::warn!(
                            "MVCC Conflict for drawing {}. Client version: {}, Server version: {}. Resolving via LWW (Server wins: client last_modified {} < server last_modified {}). Rejecting client update.",
                            item.id, item.version, row.version, item.last_modified, row.last_modified
                        );
                        // Server has a newer write. Accept server state but return success so client gets updated
                        success_uuids.push(item.id);
                        continue;
                    }
                } else {
                    item.version + 1
                }
            } else {
                item.version
            };

            tracing::info!(
                "Applying drawing upsert for {}. Next version: {}, is_deleted: {}",
                item.id,
                next_version,
                item.is_deleted
            );

            sqlx::query!(
                "INSERT INTO drawings (id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7::text::sync_state, $8, $9) \
                 ON CONFLICT (id) DO UPDATE SET \
                     client_uuid = EXCLUDED.client_uuid, \
                     version = EXCLUDED.version, \
                     is_deleted = EXCLUDED.is_deleted, \
                     last_modified = EXCLUDED.last_modified, \
                     sync_state = EXCLUDED.sync_state, \
                     data = EXCLUDED.data",
                item.id,
                user_id,
                client_id,
                next_version,
                item.is_deleted,
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

pub async fn fetch_drawings_for_response(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    last_synced_at: Option<DateTime<Utc>>,
    success_uuids: &[Uuid],
    include_remote_drawings: bool,
) -> Result<Vec<DrawingSyncItem>, AppError> {
    let last_synced_ms = last_synced_at.map(|t| t.timestamp_millis()).unwrap_or(0);

    let items = if include_remote_drawings {
        let rows = sqlx::query!(
            "SELECT id, user_id, version, is_deleted, last_modified, sync_state::TEXT as sync_state, created_at, data \
             FROM drawings \
             WHERE user_id = $1 AND ((last_modified > $2 AND client_uuid != $3) OR id = ANY($4))",
            user_id,
            last_synced_ms,
            client_id,
            success_uuids
        )
        .fetch_all(&mut **tx)
        .await?;

        rows.into_iter()
            .map(|row| DrawingSyncItem {
                id: row.id,
                user_id: Some(row.user_id),
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
            "SELECT id, user_id, version, is_deleted, last_modified, sync_state::TEXT as sync_state, created_at, data \
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
                user_id: Some(row.user_id),
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

