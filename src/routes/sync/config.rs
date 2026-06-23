use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

pub async fn process_config_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    changes: &[ConfigChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
) -> Result<(), AppError> {
    for change in changes {
        let change_id = change.id;
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing config {}", change_id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<ConfigData>(data.clone()) {
                        Ok(item) => {
                            // Fetch existing config from database
                            let existing = sqlx::query!(
                                "SELECT version, last_modified, value FROM configs WHERE id = $1 AND user_id = $2",
                                change_id,
                                user_id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            let next_version = if let Some(ref row) = existing {
                                if item.version == row.version {
                                    row.version + 1
                                } else if item.version < row.version {
                                    if item.last_modified >= row.last_modified {
                                        row.version + 1
                                    } else {
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

                            sqlx::query!(
                                "INSERT INTO configs (id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, key, value) \
                                 VALUES ($1, $2, $3, $4, $5, $6, $7::text::sync_state, $8, $9) \
                                 ON CONFLICT (id) DO UPDATE SET \
                                     client_uuid = EXCLUDED.client_uuid, \
                                     version = EXCLUDED.version, \
                                     is_deleted = EXCLUDED.is_deleted, \
                                     last_modified = EXCLUDED.last_modified, \
                                     sync_state = EXCLUDED.sync_state, \
                                     value = EXCLUDED.value",
                                change_id,
                                user_id,
                                client_id,
                                next_version,
                                item.is_deleted,
                                item.last_modified,
                                "SYNCED",
                                item.key,
                                item.value
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
                            tracing::error!("Failed to deserialize ConfigData for config {}: {:?}", change_id, err);
                            return Err(AppError::Serialization(err));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let existing = sqlx::query!(
                        "SELECT version FROM configs WHERE id = $1 AND user_id = $2",
                        change_id,
                        user_id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = existing {
                        let next_version = row.version + 1;
                        sqlx::query!(
                            "UPDATE configs SET version = $1, client_uuid = $2, sync_state = 'SYNCED' WHERE id = $3 AND user_id = $4",
                            next_version,
                            client_id,
                            change_id,
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
                    "SELECT version FROM configs WHERE id = $1 AND user_id = $2",
                    change_id,
                    user_id
                )
                .fetch_optional(&mut **tx)
                .await?;

                if let Some(row) = existing {
                    let next_version = row.version + 1;
                    sqlx::query!(
                        "UPDATE configs SET is_deleted = TRUE, version = $1, client_uuid = $2, sync_state = 'PENDING_DELETE' WHERE id = $3 AND user_id = $4",
                        next_version,
                        client_id,
                        change_id,
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

pub async fn fetch_remote_config_mutations(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    client_id: &Uuid,
    last_synced_at: Option<DateTime<Utc>>,
) -> Result<Vec<ConfigChangeDelta>, AppError> {
    let mut remote_changes = Vec::new();
    let last_synced_ms = last_synced_at.map(|t| t.timestamp_millis()).unwrap_or(0);

    let rows = sqlx::query!(
        "SELECT id, user_id, client_uuid, version, is_deleted, last_modified, sync_state::TEXT as sync_state, key, value \
         FROM configs \
         WHERE user_id = $1 AND last_modified > $2 AND client_uuid != $3",
        user_id,
        last_synced_ms,
        client_id
    )
    .fetch_all(&mut **tx)
    .await?;

    for row in rows {
        let item_data = ConfigData {
            id: row.id,
            user_id: row.user_id,
            client_uuid: row.client_uuid,
            version: row.version,
            is_deleted: row.is_deleted,
            last_modified: row.last_modified,
            sync_state: row.sync_state.clone().unwrap_or_else(|| "SYNCED".to_string()),
            key: row.key,
            value: row.value,
        };

        let data_val = serde_json::to_value(&item_data)?;

        remote_changes.push(ConfigChangeDelta {
            id: row.id,
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
