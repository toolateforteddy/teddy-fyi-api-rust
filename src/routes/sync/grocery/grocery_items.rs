use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn process_grocery_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[GroceryChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
    remote_changes: &mut Vec<GroceryChangeDelta>,
) -> Result<(), AppError> {
    let change_ids: Vec<String> = changes.iter().map(|c| c.id.clone()).collect();
    let existing_records = sqlx::query!(
        r#"SELECT id, name, quantity, "isBought" as is_bought, "createdAt" as created_at, position, "categoryId" as category_id, "timesBought" as times_bought, "userId" as user_id, "isActive" as is_active, "listId" as list_id, unit, notes, version, is_deleted, sync_state FROM grocery_items WHERE id = ANY($1)"#,
        &change_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let existing_map: std::collections::HashMap<String, _> = existing_records
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();

    let mut list_ids = std::collections::HashSet::new();
    for change in changes {
        if let Some(ref data) = change.data {
            if let Ok(item) = serde_json::from_value::<GroceryItemData>(data.clone()) {
                if let Some(ref list_id) = item.list_id {
                    list_ids.insert(list_id.clone());
                }
            }
        }
        if let Some(row) = existing_map.get(&change.id) {
            if let Some(ref list_id) = row.list_id {
                list_ids.insert(list_id.clone());
            }
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

    let existing_store_infos = sqlx::query!(
        r#"SELECT "groceryItemId" as grocery_item_id, "storeId" as store_id FROM grocery_item_store_info WHERE "groceryItemId" = ANY($1)"#,
        &change_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let mut existing_store_info_set = std::collections::HashSet::new();
    for row in existing_store_infos {
        existing_store_info_set.insert((row.grocery_item_id, row.store_id));
    }

    for change in changes {
        let string_id = change.id.clone();
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing grocery item {}", change.id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    if let Some(row) = existing_map.get(&change.id) {
                        let mut authorized = false;
                        if let Some(ref list_id) = row.list_id {
                            if member_lists_set.contains(list_id) {
                                authorized = true;
                            }
                        } else {
                            if row.user_id.as_deref() == Some(user_id) {
                                authorized = true;
                            }
                        }
                        if !authorized {
                            return Err(AppError::Forbidden(format!("User is not authorized to update grocery item {}", change.id)));
                        }

                        let item_data = GroceryItemData {
                            id: change.id.clone(),
                            name: row.name.clone(),
                            quantity: row.quantity.clone(),
                            is_bought: row.is_bought,
                            created_at: row.created_at,
                            position: row.position,
                            category_id: row.category_id.clone(),
                            times_bought: row.times_bought,
                            user_id: row.user_id.clone(),
                            is_active: row.is_active,
                            list_id: row.list_id.clone(),
                            unit: row.unit.clone(),
                            notes: row.notes.clone(),
                            version: row.version,
                            is_deleted: row.is_deleted,
                            sync_state: row.sync_state.clone(),
                        };
                        let data_val = serde_json::to_value(&item_data)?;
                        remote_changes.push(GroceryChangeDelta {
                            id: change.id.clone(),
                            operation_type: OperationType::Update,
                            version: row.version,
                            data: Some(data_val),
                        });
                        success_ids.push(change.id.clone());
                    }
                    continue;
                }

                if let Some(ref data) = change.data {
                    match serde_json::from_value::<GroceryItemData>(data.clone()) {
                        Ok(item) => {
                            // Verify permission: User must belong to the list specified by list_id (if any)
                            if let Some(ref list_id) = item.list_id {
                                if !member_lists_set.contains(list_id) {
                                    return Err(AppError::Forbidden(format!(
                                        "User is not a member of list {}",
                                        list_id
                                    )));
                                }
                            }

                            let record = existing_map.get(&change.id);

                            if record.is_some() && matches!(change.operation_type, OperationType::Update) {
                                // For Update, verify existing item's list membership too
                                if let Some(row) = record {
                                    if let Some(ref list_id) = row.list_id {
                                        if !member_lists_set.contains(list_id) {
                                            return Err(AppError::Forbidden(format!(
                                                "User is not authorized to update grocery item in list {}",
                                                list_id
                                            )));
                                        }
                                    } else {
                                        if row.user_id.as_deref() != Some(user_id) {
                                            return Err(AppError::Forbidden(format!("User is not authorized to update grocery item {}", change.id)));
                                        }
                                    }
                                }
                            }

                            let next_version = if let Some(row) = record {
                                if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for grocery {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.id, change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
                            };

                            sqlx::query!(
                                r#"
                                INSERT INTO grocery_items (
                                    id, name, quantity, "isBought", "createdAt", position, "categoryId",
                                    "timesBought", "userId", "isActive", "listId", unit, notes, version,
                                    is_deleted, sync_state, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
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
                                    sync_state = EXCLUDED.sync_state,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                                item.id,
                                item.name,
                                item.quantity,
                                item.is_bought,
                                item.created_at,
                                item.position,
                                item.category_id,
                                item.times_bought,
                                user_id, // override with authenticated user_id
                                item.is_active,
                                item.list_id,
                                item.unit,
                                item.notes,
                                next_version,
                                item.is_deleted,
                                "SYNCED",
                                server_timestamp,
                                client_id
                            )
                            .execute(&mut **tx)
                            .await?;

                            // Auto-populate store mapping
                            let existing_mappings = sqlx::query!(
                                r#"
                                SELECT DISTINCT gsi."storeId" as store_id, gsi.price, gsi."isAvailable" as is_available
                                FROM grocery_item_store_info gsi
                                JOIN grocery_items gi ON gsi."groceryItemId" = gi.id
                                JOIN grocery_list_members glm ON gi."listId" = glm."listId"
                                WHERE LOWER(gi.name) = LOWER($1)
                                  AND glm."userId" = $2
                                  AND gi.is_deleted = FALSE
                                  AND gsi.is_deleted = FALSE
                                "#,
                                item.name,
                                user_id
                            )
                            .fetch_all(&mut **tx)
                            .await?;

                            for mapping in existing_mappings {
                                let exists = existing_store_info_set.contains(&(item.id.clone(), mapping.store_id.clone()));

                                if !exists {
                                    sqlx::query!(
                                        r#"
                                        INSERT INTO grocery_item_store_info (
                                            "groceryItemId", "storeId", price, "isAvailable", "userId", version, is_deleted, sync_state, updated_at, updated_by_client
                                        ) VALUES ($1, $2, $3, $4, $5, 1, FALSE, 'SYNCED', $6, NULL)
                                        "#,
                                        item.id,
                                        mapping.store_id,
                                        mapping.price,
                                        mapping.is_available,
                                        user_id,
                                        server_timestamp
                                    )
                                    .execute(&mut **tx)
                                    .await?;
                                }
                            }

                            upload_status.push(SuccessResult {
                                id: string_id.clone(),
                                version: next_version,
                                sync_state: "SYNCED".to_string(),
                            });
                            success_ids.push(string_id);
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize GroceryItemData for grocery {}: {:?}. Data: {:?}",
                                change.id,
                                err,
                                data
                            );
                            return Err(AppError::Serialization(err));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let record = existing_map.get(&change.id);

                    if let Some(ref row) = record {
                        if let Some(ref list_id) = row.list_id {
                            if !member_lists_set.contains(list_id) {
                                return Err(AppError::Forbidden(format!(
                                    "User is not authorized to update grocery item in list {}",
                                    list_id
                                )));
                            }
                        } else {
                            if row.user_id.as_deref() != Some(user_id) {
                                return Err(AppError::Forbidden(format!("User is not authorized to update grocery item {}", change.id)));
                            }
                        }

                        let next_version = row.version + 1;
                        sqlx::query!(
                            "UPDATE grocery_items SET version = $1, updated_at = $2, updated_by_client = $3, sync_state = 'SYNCED' WHERE id = $4",
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
                let record = existing_map.get(&change.id);

                if let Some(ref row) = record {
                    if row.is_deleted {
                        upload_status.push(SuccessResult {
                            id: string_id.clone(),
                            version: row.version,
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(string_id);
                        continue;
                    }

                    if let Some(ref list_id) = row.list_id {
                        if !member_lists_set.contains(list_id) {
                            return Err(AppError::Forbidden(format!(
                                "User is not authorized to delete grocery item in list {}",
                                list_id
                            )));
                        }
                    } else {
                        if row.user_id.as_deref() != Some(user_id) {
                            return Err(AppError::Forbidden(format!("User is not authorized to delete grocery item {}", change.id)));
                        }
                    }

                    let row_result = sqlx::query!(
                        "UPDATE grocery_items SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 WHERE id = $3 RETURNING version",
                        server_timestamp,
                        client_id,
                        change.id
                    )
                    .fetch_one(&mut **tx)
                    .await?;

                    upload_status.push(SuccessResult {
                        id: string_id.clone(),
                        version: row_result.version,
                        sync_state: "SYNCED".to_string(),
                    });
                    success_ids.push(string_id);
                }
            }
        }
    }
    Ok(())
}
