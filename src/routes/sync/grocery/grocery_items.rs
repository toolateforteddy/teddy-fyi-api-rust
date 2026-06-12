use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn process_grocery_changes(
    tx: &mut Transaction<'_, Postgres>,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[GroceryChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
) -> Result<(), AppError> {
    for change in changes {
        let string_id = change.id.to_string();
        match change.operation_type {
            OperationType::Insert => {
                tracing::info!("Inserting grocery {}", change.id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<GroceryItemData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!(
                                "SELECT version FROM grocery_items WHERE id = $1",
                                change.id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            let next_version = if let Some(row) = record {
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
                            };

                            sqlx::query(
                                r#"
                                INSERT INTO grocery_items (
                                    id, name, quantity, "isBought", "createdAt", position, "categoryId",
                                    "timesBought", "userId", "isActive", "listId", unit, notes, version,
                                    is_deleted, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
                                ON CONFLICT (id) DO UPDATE SET
                                    name = EXCLUDED.name,
                                    quantity = EXCLUDED.quantity,
                                    "isBought" = EXCLUDED."isBought",
                                    "createdAt" = EXCLUDED."createdAt",
                                    position = EXCLUDED.position,
                                    "categoryId" = EXCLUDED."categoryId",
                                    "timesBought" = EXCLUDED."timesBought",
                                    "userId" = EXCLUDED."userId",
                                    "isActive" = EXCLUDED."isActive",
                                    "listId" = EXCLUDED."listId",
                                    unit = EXCLUDED.unit,
                                    notes = EXCLUDED.notes,
                                    version = EXCLUDED.version,
                                    is_deleted = EXCLUDED.is_deleted,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                            )
                            .bind(item.id)
                            .bind(&item.name)
                            .bind(&item.quantity)
                            .bind(item.is_bought)
                            .bind(item.created_at)
                            .bind(item.position)
                            .bind(item.category_id)
                            .bind(item.times_bought)
                            .bind(&item.user_id)
                            .bind(item.is_active)
                            .bind(&item.list_id)
                            .bind(&item.unit)
                            .bind(&item.notes)
                            .bind(next_version)
                            .bind(item.is_deleted)
                            .bind(server_timestamp)
                            .bind(client_id)
                            .execute(&mut **tx)
                            .await?;

                            upload_status.push(SuccessResult {
                                id: string_id.clone(),
                                version: next_version,
                                sync_state: "SYNCED".to_string(),
                            });
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize GroceryItemData for grocery {}: {:?}",
                                change.id,
                                err
                            );
                        }
                    }
                }
                success_ids.push(string_id);
            }
            OperationType::Update => {
                tracing::info!("Updating grocery {}", change.id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<GroceryItemData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!(
                                "SELECT version FROM grocery_items WHERE id = $1",
                                change.id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            let next_version = if let Some(row) = record {
                                if change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for grocery {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.id, change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, change.version) + 1
                            } else {
                                change.version
                            };

                            sqlx::query(
                                r#"
                                INSERT INTO grocery_items (
                                    id, name, quantity, "isBought", "createdAt", position, "categoryId",
                                    "timesBought", "userId", "isActive", "listId", unit, notes, version,
                                    is_deleted, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
                                ON CONFLICT (id) DO UPDATE SET
                                    name = EXCLUDED.name,
                                    quantity = EXCLUDED.quantity,
                                    "isBought" = EXCLUDED."isBought",
                                    "createdAt" = EXCLUDED."createdAt",
                                    position = EXCLUDED.position,
                                    "categoryId" = EXCLUDED."categoryId",
                                    "timesBought" = EXCLUDED."timesBought",
                                    "userId" = EXCLUDED."userId",
                                    "isActive" = EXCLUDED."isActive",
                                    "listId" = EXCLUDED."listId",
                                    unit = EXCLUDED.unit,
                                    notes = EXCLUDED.notes,
                                    version = EXCLUDED.version,
                                    is_deleted = EXCLUDED.is_deleted,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                            )
                            .bind(item.id)
                            .bind(&item.name)
                            .bind(&item.quantity)
                            .bind(item.is_bought)
                            .bind(item.created_at)
                            .bind(item.position)
                            .bind(item.category_id)
                            .bind(item.times_bought)
                            .bind(&item.user_id)
                            .bind(item.is_active)
                            .bind(&item.list_id)
                            .bind(&item.unit)
                            .bind(&item.notes)
                            .bind(next_version)
                            .bind(item.is_deleted)
                            .bind(server_timestamp)
                            .bind(client_id)
                            .execute(&mut **tx)
                            .await?;

                            upload_status.push(SuccessResult {
                                id: string_id.clone(),
                                version: next_version,
                                sync_state: "SYNCED".to_string(),
                            });
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize GroceryItemData for grocery {}: {:?}",
                                change.id,
                                err
                            );
                        }
                    }
                } else {
                    let record =
                        sqlx::query!("SELECT version FROM grocery_items WHERE id = $1", change.id)
                            .fetch_optional(&mut **tx)
                            .await?;

                    if let Some(row) = record {
                        let next_version = row.version + 1;
                        if change.version < row.version {
                            tracing::warn!(
                                "MVCC Conflict for grocery {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                change.id, change.version, row.version
                            );
                        }

                        sqlx::query!(
                            "UPDATE grocery_items SET version = $1, updated_at = $2, updated_by_client = $3 WHERE id = $4",
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
                    }
                }
                success_ids.push(string_id);
            }
            OperationType::Delete => {
                let row = sqlx::query!(
                    "UPDATE grocery_items SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 WHERE id = $3 RETURNING version",
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
