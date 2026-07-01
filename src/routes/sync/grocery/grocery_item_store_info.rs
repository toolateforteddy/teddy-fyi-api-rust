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
    remote_changes: &mut Vec<GroceryItemStoreInfoChangeDelta>,
) -> Result<(), AppError> {
    let parent_item_ids: Vec<String> = changes.iter().map(|c| c.grocery_item_id.clone()).collect();
    let parent_items = sqlx::query!(
        r#"SELECT id, "userId" as user_id, "listId" as list_id, is_deleted FROM grocery_items WHERE id = ANY($1)"#,
        &parent_item_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let parent_items_map: std::collections::HashMap<String, _> = parent_items
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();

    let mut list_ids = std::collections::HashSet::new();
    for row in parent_items_map.values() {
        if let Some(ref list_id) = row.list_id {
            list_ids.insert(list_id.clone());
        }
    }
    let list_ids_vec: Vec<String> = list_ids.into_iter().collect();

    let membership_records = sqlx::query!(
        r#"SELECT "listId" as list_id FROM grocery_list_members WHERE "userId" = $1 AND "listId" = ANY($2) AND is_deleted = FALSE"#,
        user_id,
        &list_ids_vec
    )
    .fetch_all(&mut **tx)
    .await?;

    let member_lists_set: std::collections::HashSet<String> = membership_records
        .into_iter()
        .map(|r| r.list_id)
        .collect();

    let existing_infos = sqlx::query!(
        r#"SELECT "groceryItemId" as grocery_item_id, "storeId" as store_id, price, "isAvailable" as is_available, "userId" as user_id, version, is_deleted, sync_state FROM grocery_item_store_info WHERE "groceryItemId" = ANY($1)"#,
        &parent_item_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut existing_map = std::collections::HashMap::new();
    for row in existing_infos {
        existing_map.insert((row.grocery_item_id.clone(), row.store_id.clone()), row);
    }

    for change in changes {
        let string_id = if !change.id.is_empty() {
            change.id.clone()
        } else {
            format!("{}-{}", change.grocery_item_id, change.store_id)
        };
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!(
                    "Processing grocery item store info for grocery {}, store {}",
                    change.grocery_item_id,
                    change.store_id
                );

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    let parent = parent_items_map.get(&change.grocery_item_id);
                    if let Some(parent) = parent {
                        let mut authorized = parent.user_id.as_deref() == Some(user_id);
                        if !authorized {
                            if let Some(ref list_id) = parent.list_id {
                                if member_lists_set.contains(list_id) {
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

                    let existing = existing_map.get(&(change.grocery_item_id.clone(), change.store_id.clone()));

                    if let Some(row) = existing {
                        let item_data = GroceryItemStoreInfoData {
                            id: string_id.clone(),
                            grocery_item_id: change.grocery_item_id.clone(),
                            store_id: change.store_id.clone(),
                            price: row.price,
                            is_available: row.is_available,
                            user_id: row.user_id.clone(),
                            version: row.version,
                            is_deleted: row.is_deleted,
                            sync_state: row.sync_state.clone(),
                        };
                        let data_val = serde_json::to_value(&item_data)?;
                        remote_changes.push(GroceryItemStoreInfoChangeDelta {
                            id: string_id.clone(),
                            grocery_item_id: change.grocery_item_id.clone(),
                            store_id: change.store_id.clone(),
                            operation_type: OperationType::Update,
                            version: row.version,
                            data: Some(data_val),
                        });
                        success_ids.push(string_id);
                    }
                    continue;
                }

                if let Some(ref data) = change.data {
                    match serde_json::from_value::<GroceryItemStoreInfoData>(data.clone()) {
                        Ok(item) => {
                            let record = existing_map.get(&(item.grocery_item_id.clone(), item.store_id.clone()));

                            if record.is_some() {
                                let parent = parent_items_map.get(&item.grocery_item_id);
                                if let Some(parent) = parent {
                                    let mut authorized = parent.user_id.as_deref() == Some(user_id);
                                    if !authorized {
                                        if let Some(ref list_id) = parent.list_id {
                                            if member_lists_set.contains(list_id) {
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
                    let parent = parent_items_map.get(&change.grocery_item_id);
                    if let Some(parent) = parent {
                        let mut authorized = parent.user_id.as_deref() == Some(user_id);
                        if !authorized {
                            if let Some(ref list_id) = parent.list_id {
                                if member_lists_set.contains(list_id) {
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

                    let record = existing_map.get(&(change.grocery_item_id.clone(), change.store_id.clone()));

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
                let existing_info = existing_map.get(&(change.grocery_item_id.clone(), change.store_id.clone()));

                if let Some(info) = existing_info {
                    if info.is_deleted {
                        upload_status.push(SuccessResult {
                            id: string_id.clone(),
                            version: info.version,
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(string_id);
                        continue;
                    }
                }

                let parent = parent_items_map.get(&change.grocery_item_id);
                if let Some(parent) = parent {
                    let mut authorized = parent.is_deleted;
                    if !authorized {
                        authorized = parent.user_id.as_deref() == Some(user_id);
                        if !authorized {
                            if let Some(ref list_id) = parent.list_id {
                                if member_lists_set.contains(list_id) {
                                    authorized = true;
                                }
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
