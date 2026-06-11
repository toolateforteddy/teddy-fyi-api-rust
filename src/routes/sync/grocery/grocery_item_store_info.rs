use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn process_grocery_item_store_info_changes(
    tx: &mut Transaction<'_, Postgres>,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[GroceryItemStoreInfoChangeDelta],
    success_ids: &mut Vec<String>,
) -> Result<(), AppError> {
    for change in changes {
        let string_id = format!("{}-{}", change.grocery_item_id, change.store_id);
        match change.operation_type {
            OperationType::Insert => {
                tracing::info!(
                    "Inserting grocery item store info for grocery {}, store {}",
                    change.grocery_item_id,
                    change.store_id
                );
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<GroceryItemStoreInfoData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!(
                                r#"SELECT version FROM grocery_item_store_info WHERE "groceryItemId" = $1 AND "storeId" = $2"#,
                                change.grocery_item_id, change.store_id
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
                                INSERT INTO grocery_item_store_info (
                                    "groceryItemId", "storeId", price, "isAvailable", "userId", version, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                                ON CONFLICT ("groceryItemId", "storeId") DO UPDATE SET
                                    price = EXCLUDED.price,
                                    "isAvailable" = EXCLUDED."isAvailable",
                                    "userId" = EXCLUDED."userId",
                                    version = EXCLUDED.version,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                            )
                            .bind(item.grocery_item_id)
                            .bind(item.store_id)
                            .bind(item.price)
                            .bind(item.is_available)
                            .bind(&item.user_id)
                            .bind(next_version)
                            .bind(server_timestamp)
                            .bind(client_id)
                            .execute(&mut **tx)
                            .await?;
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize GroceryItemStoreInfoData: {:?}",
                                err
                            );
                        }
                    }
                }
                success_ids.push(string_id);
            }
            OperationType::Update => {
                tracing::info!(
                    "Updating grocery item store info for grocery {}, store {}",
                    change.grocery_item_id,
                    change.store_id
                );
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<GroceryItemStoreInfoData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!(
                                r#"SELECT version FROM grocery_item_store_info WHERE "groceryItemId" = $1 AND "storeId" = $2"#,
                                change.grocery_item_id, change.store_id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            let next_version = if let Some(row) = record {
                                if change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for store info. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, change.version) + 1
                            } else {
                                change.version
                            };

                            sqlx::query(
                                r#"
                                INSERT INTO grocery_item_store_info (
                                    "groceryItemId", "storeId", price, "isAvailable", "userId", version, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                                ON CONFLICT ("groceryItemId", "storeId") DO UPDATE SET
                                    price = EXCLUDED.price,
                                    "isAvailable" = EXCLUDED."isAvailable",
                                    "userId" = EXCLUDED."userId",
                                    version = EXCLUDED.version,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                            )
                            .bind(item.grocery_item_id)
                            .bind(item.store_id)
                            .bind(item.price)
                            .bind(item.is_available)
                            .bind(&item.user_id)
                            .bind(next_version)
                            .bind(server_timestamp)
                            .bind(client_id)
                            .execute(&mut **tx)
                            .await?;
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize GroceryItemStoreInfoData: {:?}",
                                err
                            );
                        }
                    }
                } else {
                    let record = sqlx::query!(
                        r#"SELECT version FROM grocery_item_store_info WHERE "groceryItemId" = $1 AND "storeId" = $2"#,
                        change.grocery_item_id, change.store_id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = record {
                        let next_version = row.version + 1;
                        if change.version < row.version {
                            tracing::warn!(
                                "MVCC Conflict for store info. Client version: {}, Server version: {}. Resolving via LWW.",
                                change.version, row.version
                            );
                        }

                        sqlx::query!(
                            r#"UPDATE grocery_item_store_info SET version = $1, updated_at = $2, updated_by_client = $3 WHERE "groceryItemId" = $4 AND "storeId" = $5"#,
                            next_version,
                            server_timestamp,
                            client_id,
                            change.grocery_item_id,
                            change.store_id
                        )
                        .execute(&mut **tx)
                        .await?;
                    }
                }
                success_ids.push(string_id);
            }
            OperationType::Delete => {
                sqlx::query!(
                    r#"DELETE FROM grocery_item_store_info WHERE "groceryItemId" = $1 AND "storeId" = $2"#,
                    change.grocery_item_id,
                    change.store_id
                )
                .execute(&mut **tx)
                .await?;
                success_ids.push(string_id);
            }
        }
    }
    Ok(())
}
