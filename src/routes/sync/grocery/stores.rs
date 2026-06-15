use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn process_store_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[StoreChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
) -> Result<(), AppError> {
    for change in changes {
        let string_id = change.id.to_string();
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing store {}", change.id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<StoreData>(data.clone()) {
                        Ok(item) => {
                            let record =
                                sqlx::query!("SELECT version FROM stores WHERE id = $1", item.id)
                                    .fetch_optional(&mut **tx)
                                    .await?;

                            if record.is_some() {
                                let store_owner = sqlx::query!(
                                    r#"SELECT "userId" as user_id FROM stores WHERE id = $1"#,
                                    item.id
                                )
                                .fetch_one(&mut **tx)
                                .await?;
                                if store_owner.user_id.as_deref() != Some(user_id) {
                                    return Err(AppError::Forbidden(format!("User is not authorized to update store {}", item.id)));
                                }
                            }

                            let next_version = if let Some(row) = record {
                                if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for store {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.id, change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
                            };

                            sqlx::query!(
                                r#"
                                INSERT INTO stores (
                                    id, name, position, "isDefaultSupported", "userId", version, is_deleted, sync_state, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                                ON CONFLICT (id) DO UPDATE SET
                                    name = EXCLUDED.name,
                                    position = EXCLUDED.position,
                                    "isDefaultSupported" = EXCLUDED."isDefaultSupported",
                                    "userId" = EXCLUDED."userId",
                                    version = EXCLUDED.version,
                                    is_deleted = EXCLUDED.is_deleted,
                                    sync_state = EXCLUDED.sync_state,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                                item.id,
                                item.name,
                                item.position,
                                item.is_default_supported,
                                user_id, // override with authenticated user_id
                                next_version,
                                item.is_deleted,
                                "SYNCED",
                                server_timestamp,
                                client_id
                            )
                            .execute(&mut **tx)
                            .await?;

                            upload_status.push(SuccessResult {
                                id: string_id.clone(),
                                version: next_version,
                                sync_state: "SYNCED".to_string(),
                            });
                            success_ids.push(string_id);
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize StoreData for store {}: {:?}",
                                change.id,
                                err
                            );
                            return Err(AppError::Serialization(err));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let store_owner = sqlx::query!(
                        r#"SELECT "userId" as user_id FROM stores WHERE id = $1"#,
                        change.id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;
                    if let Some(row) = store_owner {
                        if row.user_id.as_deref() != Some(user_id) {
                            return Err(AppError::Forbidden(format!("User is not authorized to update store {}", change.id)));
                        }
                    }

                    let record =
                        sqlx::query!("SELECT version FROM stores WHERE id = $1", change.id)
                            .fetch_optional(&mut **tx)
                            .await?;

                    if let Some(row) = record {
                        let next_version = row.version + 1;
                        sqlx::query!(
                            "UPDATE stores SET version = $1, updated_at = $2, updated_by_client = $3, sync_state = 'SYNCED' WHERE id = $4",
                            next_version,
                            server_timestamp,
                            client_id,
                            change.id
                        )
                        .execute(&mut **tx)
                        .await?;

                        upload_status.push(SuccessResult {
                            id: string_id.clone(),
                            version: next_version,
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(string_id);
                    }
                }
            }
            OperationType::Delete => {
                let store_owner = sqlx::query!(
                    r#"SELECT "userId" as user_id FROM stores WHERE id = $1"#,
                    change.id
                )
                .fetch_optional(&mut **tx)
                .await?;
                if let Some(row) = store_owner {
                    if row.user_id.as_deref() != Some(user_id) {
                        return Err(AppError::Forbidden(format!("User is not authorized to delete store {}", change.id)));
                    }
                }

                let row = sqlx::query!(
                    "UPDATE stores SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 WHERE id = $3 RETURNING version",
                    server_timestamp,
                    client_id,
                    change.id
                )
                .fetch_one(&mut **tx)
                .await?;

                upload_status.push(SuccessResult {
                    id: string_id.clone(),
                    version: row.version,
                    sync_state: "SYNCED".to_string(),
                });
                success_ids.push(string_id);
            }
        }
    }
    Ok(())
}
