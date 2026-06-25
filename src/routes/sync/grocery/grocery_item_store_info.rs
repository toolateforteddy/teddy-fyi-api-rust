use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn process_grocery_item_store_info_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[GroceryItemStoreInfoChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
) -> Result<(), AppError> {
    for change in changes {
        let string_id = format!("{}-{}", change.grocery_item_id, change.store_id);
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!(
                    "Processing grocery item store info for grocery {}, store {}",
                    change.grocery_item_id,
                    change.store_id
                );
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<GroceryItemStoreInfoData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!(
                                r#"SELECT version FROM grocery_item_store_info WHERE "groceryItemId" = $1 AND "storeId" = $2"#,
                                item.grocery_item_id, item.store_id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            if record.is_some() {
                                let parent_item = sqlx::query!(
                                    r#"SELECT "userId" as user_id, "listId" as list_id FROM grocery_items WHERE id = $1"#,
                                    item.grocery_item_id
                                )
                                .fetch_optional(&mut **tx)
                                .await?;
                                if let Some(parent) = parent_item {
                                    let mut authorized = parent.user_id.as_deref() == Some(user_id);
                                    if !authorized {
                                        if let Some(ref list_id) = parent.list_id {
                                            let is_member = sqlx::query!(
                                                r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                                                list_id,
                                                user_id
                                            )
                                            .fetch_optional(&mut **tx)
                                            .await?
                                            .is_some();
                                            if is_member {
                                                authorized = true;
                                            }
                                        }
                                    }
                                    if !authorized {
                                        return Err(AppError::Forbidden(format!(
                                            "User is not authorized to update store info for item {} store {}",
                                            item.grocery_item_id, item.store_id
                                        )));
                                    }
                                } else {
                                    return Err(AppError::Forbidden(format!("Parent grocery item not found: {}", item.grocery_item_id)));
                                }
                            }

                            let next_version = if let Some(row) = record {
                                if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for store info. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
                            };

                            sqlx::query!(
                                r#"
                                INSERT INTO grocery_item_store_info (
                                    "groceryItemId", "storeId", price, "isAvailable", "userId", version, is_deleted, sync_state, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                                ON CONFLICT ("groceryItemId", "storeId") DO UPDATE SET
                                    price = EXCLUDED.price,
                                    "isAvailable" = EXCLUDED."isAvailable",
                                    "userId" = EXCLUDED."userId",
                                    version = EXCLUDED.version,
                                    is_deleted = EXCLUDED.is_deleted,
                                    sync_state = EXCLUDED.sync_state,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                                item.grocery_item_id,
                                item.store_id,
                                item.price,
                                item.is_available,
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
                                "Failed to deserialize GroceryItemStoreInfoData for item {}-{}: {:?}. Data: {:?}",
                                change.grocery_item_id, change.store_id,
                                err,
                                data
                            );
                            return Err(AppError::Serialization(err));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let parent_item = sqlx::query!(
                        r#"SELECT "userId" as user_id, "listId" as list_id FROM grocery_items WHERE id = $1"#,
                        change.grocery_item_id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;
                    if let Some(parent) = parent_item {
                        let mut authorized = parent.user_id.as_deref() == Some(user_id);
                        if !authorized {
                            if let Some(ref list_id) = parent.list_id {
                                let is_member = sqlx::query!(
                                    r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                                    list_id,
                                    user_id
                                )
                                .fetch_optional(&mut **tx)
                                .await?
                                .is_some();
                                if is_member {
                                    authorized = true;
                                }
                            }
                        }
                        if !authorized {
                            return Err(AppError::Forbidden(format!(
                                "User is not authorized to update store info for item {} store {}",
                                change.grocery_item_id, change.store_id
                            )));
                        }
                    } else {
                        return Err(AppError::Forbidden(format!("Parent grocery item not found: {}", change.grocery_item_id)));
                    }

                    let record = sqlx::query!(
                        r#"SELECT version FROM grocery_item_store_info WHERE "groceryItemId" = $1 AND "storeId" = $2"#,
                        change.grocery_item_id, change.store_id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = record {
                        let next_version = row.version + 1;
                        sqlx::query!(
                            r#"UPDATE grocery_item_store_info SET version = $1, updated_at = $2, updated_by_client = $3, sync_state = 'SYNCED' WHERE "groceryItemId" = $4 AND "storeId" = $5"#,
                            next_version,
                            server_timestamp,
                            client_id,
                            change.grocery_item_id,
                            change.store_id
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
                let parent_item = sqlx::query!(
                    r#"SELECT "userId" as user_id, "listId" as list_id FROM grocery_items WHERE id = $1"#,
                    change.grocery_item_id
                )
                .fetch_optional(&mut **tx)
                .await?;
                if let Some(parent) = parent_item {
                    let mut authorized = parent.user_id.as_deref() == Some(user_id);
                    if !authorized {
                        if let Some(ref list_id) = parent.list_id {
                            let is_member = sqlx::query!(
                                r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                                list_id,
                                user_id
                            )
                            .fetch_optional(&mut **tx)
                            .await?
                            .is_some();
                            if is_member {
                                authorized = true;
                            }
                        }
                    }
                    if !authorized {
                        return Err(AppError::Forbidden(format!(
                            "User is not authorized to delete store info for item {} store {}",
                            change.grocery_item_id, change.store_id
                        )));
                    }
                } else {
                    return Err(AppError::Forbidden(format!("Parent grocery item not found: {}", change.grocery_item_id)));
                }

                let row = sqlx::query!(
                    r#"UPDATE grocery_item_store_info SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 WHERE "groceryItemId" = $3 AND "storeId" = $4 RETURNING version"#,
                    server_timestamp,
                    client_id,
                    change.grocery_item_id,
                    change.store_id
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
