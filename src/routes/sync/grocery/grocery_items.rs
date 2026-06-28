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
    for change in changes {
        let string_id = change.id.clone();
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing grocery item {}", change.id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    let existing = sqlx::query!(
                        r#"SELECT name, quantity, "isBought" as is_bought, "createdAt" as created_at, position, "categoryId" as category_id, "timesBought" as times_bought, "userId" as user_id, "isActive" as is_active, "listId" as list_id, unit, notes, version, is_deleted, sync_state FROM grocery_items WHERE id = $1"#,
                        change.id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = existing {
                        let mut authorized = false;
                        if let Some(ref list_id) = row.list_id {
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
                            name: row.name,
                            quantity: row.quantity,
                            is_bought: row.is_bought,
                            created_at: row.created_at,
                            position: row.position,
                            category_id: row.category_id,
                            times_bought: row.times_bought,
                            user_id: row.user_id,
                            is_active: row.is_active,
                            list_id: row.list_id,
                            unit: row.unit,
                            notes: row.notes,
                            version: row.version,
                            is_deleted: row.is_deleted,
                            sync_state: row.sync_state,
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
                                let is_member = sqlx::query!(
                                    r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                                    list_id,
                                    user_id
                                )
                                .fetch_optional(&mut **tx)
                                .await?
                                .is_some();

                                if !is_member {
                                    return Err(AppError::Forbidden(format!(
                                        "User is not a member of list {}",
                                        list_id
                                    )));
                                }
                            }

                            let record = sqlx::query!(
                                "SELECT version FROM grocery_items WHERE id = $1",
                                change.id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            if record.is_some() && matches!(change.operation_type, OperationType::Update) {
                                // For Update, verify existing item's list membership too
                                let existing_item = sqlx::query!(
                                    r#"SELECT "userId" as user_id, "listId" as list_id FROM grocery_items WHERE id = $1"#,
                                    change.id
                                )
                                .fetch_one(&mut **tx)
                                .await?;
                                if let Some(ref list_id) = existing_item.list_id {
                                    let is_member = sqlx::query!(
                                        r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                                        list_id,
                                        user_id
                                    )
                                    .fetch_optional(&mut **tx)
                                    .await?
                                    .is_some();
                                    if !is_member {
                                        return Err(AppError::Forbidden(format!(
                                            "User is not authorized to update grocery item in list {}",
                                            list_id
                                        )));
                                    }
                                } else {
                                    if existing_item.user_id.as_deref() != Some(user_id) {
                                        return Err(AppError::Forbidden(format!("User is not authorized to update grocery item {}", change.id)));
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
                                let exists = sqlx::query!(
                                    r#"
                                    SELECT 1 as dummy FROM grocery_item_store_info
                                    WHERE "groceryItemId" = $1 AND "storeId" = $2
                                    "#,
                                    item.id,
                                    mapping.store_id
                                )
                                .fetch_optional(&mut **tx)
                                .await?;

                                if exists.is_none() {
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
                    let existing_item = sqlx::query!(
                        r#"SELECT "userId" as user_id, "listId" as list_id FROM grocery_items WHERE id = $1"#,
                        change.id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(ref row) = existing_item {
                        if let Some(ref list_id) = row.list_id {
                            let is_member = sqlx::query!(
                                r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                                list_id,
                                user_id
                            )
                            .fetch_optional(&mut **tx)
                            .await?
                            .is_some();
                            if !is_member {
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

                    let record =
                        sqlx::query!("SELECT version FROM grocery_items WHERE id = $1", change.id)
                            .fetch_optional(&mut **tx)
                            .await?;

                    if let Some(row) = record {
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
                let existing_item = sqlx::query!(
                    r#"SELECT "userId" as user_id, "listId" as list_id, is_deleted, version FROM grocery_items WHERE id = $1"#,
                    change.id
                )
                .fetch_optional(&mut **tx)
                .await?;

                if let Some(ref row) = existing_item {
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
                        let is_member = sqlx::query!(
                            r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                            list_id,
                            user_id
                        )
                        .fetch_optional(&mut **tx)
                        .await?
                        .is_some();
                        if !is_member {
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
                }

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
