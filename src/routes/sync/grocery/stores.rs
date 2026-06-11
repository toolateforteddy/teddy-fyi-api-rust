use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use crate::routes::sync::types::*;

pub async fn process_store_changes(
    tx: &mut Transaction<'_, Postgres>,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[StoreChangeDelta],
    success_ids: &mut Vec<String>,
) -> Result<(), AppError> {
    for change in changes {
        let string_id = change.id.to_string();
        match change.operation_type {
            OperationType::Insert => {
                tracing::info!("Inserting store {}", change.id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<StoreData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!("SELECT version FROM stores WHERE id = $1", change.id)
                                .fetch_optional(&mut **tx)
                                .await?;

                            let next_version = if let Some(row) = record {
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
                            };

                            sqlx::query(
                                r#"
                                INSERT INTO stores (
                                    id, name, position, "isDefaultSupported", "userId", version, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                                ON CONFLICT (id) DO UPDATE SET
                                    name = EXCLUDED.name,
                                    position = EXCLUDED.position,
                                    "isDefaultSupported" = EXCLUDED."isDefaultSupported",
                                    "userId" = EXCLUDED."userId",
                                    version = EXCLUDED.version,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                            )
                            .bind(item.id)
                            .bind(&item.name)
                            .bind(item.position)
                            .bind(item.is_default_supported)
                            .bind(&item.user_id)
                            .bind(next_version)
                            .bind(server_timestamp)
                            .bind(client_id)
                            .execute(&mut **tx)
                            .await?;
                        }
                        Err(err) => {
                            tracing::error!("Failed to deserialize StoreData for store {}: {:?}", change.id, err);
                        }
                    }
                }
                success_ids.push(string_id);
            }
            OperationType::Update => {
                tracing::info!("Updating store {}", change.id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<StoreData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!("SELECT version FROM stores WHERE id = $1", change.id)
                                .fetch_optional(&mut **tx)
                                .await?;

                            let next_version = if let Some(row) = record {
                                if change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for store {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.id, change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, change.version) + 1
                            } else {
                                change.version
                            };

                            sqlx::query(
                                r#"
                                INSERT INTO stores (
                                    id, name, position, "isDefaultSupported", "userId", version, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                                ON CONFLICT (id) DO UPDATE SET
                                    name = EXCLUDED.name,
                                    position = EXCLUDED.position,
                                    "isDefaultSupported" = EXCLUDED."isDefaultSupported",
                                    "userId" = EXCLUDED."userId",
                                    version = EXCLUDED.version,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                            )
                            .bind(item.id)
                            .bind(&item.name)
                            .bind(item.position)
                            .bind(item.is_default_supported)
                            .bind(&item.user_id)
                            .bind(next_version)
                            .bind(server_timestamp)
                            .bind(client_id)
                            .execute(&mut **tx)
                            .await?;
                        }
                        Err(err) => {
                            tracing::error!("Failed to deserialize StoreData for store {}: {:?}", change.id, err);
                        }
                    }
                } else {
                    let record = sqlx::query!("SELECT version FROM stores WHERE id = $1", change.id)
                        .fetch_optional(&mut **tx)
                        .await?;

                    if let Some(row) = record {
                        let next_version = row.version + 1;
                        if change.version < row.version {
                            tracing::warn!(
                                "MVCC Conflict for store {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                change.id, change.version, row.version
                            );
                        }

                        sqlx::query!(
                            "UPDATE stores SET version = $1, updated_at = $2, updated_by_client = $3 WHERE id = $4",
                            next_version,
                            server_timestamp,
                            client_id,
                            change.id
                        )
                        .execute(&mut **tx)
                        .await?;
                    }
                }
                success_ids.push(string_id);
            }
            OperationType::Delete => {
                sqlx::query!(
                    "DELETE FROM stores WHERE id = $1",
                    change.id
                )
                .execute(&mut **tx)
                .await?;
                success_ids.push(string_id);
            }
        }
    }
    Ok(())
}
