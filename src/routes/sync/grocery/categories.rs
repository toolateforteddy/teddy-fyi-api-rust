use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn process_category_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[CategoryChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
    remote_changes: &mut Vec<CategoryChangeDelta>,
) -> Result<(), AppError> {
    let change_ids: Vec<String> = changes.iter().map(|c| c.id.clone()).collect();
    let existing_records = sqlx::query!(
        r#"SELECT id, name, position, "userId" as user_id, icon, version, is_deleted, sync_state, "listId" as list_id FROM categories WHERE id = ANY($1)"#,
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
            if let Ok(item) = serde_json::from_value::<CategoryData>(data.clone()) {
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

    for change in changes {
        let string_id = change.id.clone();
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing category {}", change.id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    if let Some(row) = existing_map.get(&change.id) {
                        let mut authorized = row.user_id.as_deref() == Some(user_id);
                        if !authorized {
                            if let Some(ref list_id) = row.list_id {
                                if member_lists_set.contains(list_id) {
                                    authorized = true;
                                }
                            }
                        }
                        if !authorized {
                            return Err(AppError::Forbidden(format!("User is not authorized to update category {}", change.id)));
                        }

                        let item_data = CategoryData {
                            id: change.id.clone(),
                            name: row.name.clone(),
                            position: row.position,
                            user_id: row.user_id.clone(),
                            icon: row.icon.clone(),
                            version: row.version,
                            is_deleted: row.is_deleted,
                            sync_state: row.sync_state.clone(),
                            list_id: row.list_id.clone(),
                        };
                        let data_val = serde_json::to_value(&item_data)?;
                        remote_changes.push(CategoryChangeDelta {
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
                    match serde_json::from_value::<CategoryData>(data.clone()) {
                        Ok(item) => {
                            let record = existing_map.get(&change.id);

                            if let Some(ref list_id) = item.list_id {
                                if !member_lists_set.contains(list_id) {
                                    return Err(AppError::Forbidden(format!("User is not a member of list {}", list_id)));
                                }
                            }

                            if record.is_some() {
                                if let Some(row) = record {
                                    let mut authorized = row.user_id.as_deref() == Some(user_id);
                                    if !authorized {
                                        if let Some(ref list_id) = row.list_id {
                                            if member_lists_set.contains(list_id) {
                                                authorized = true;
                                            }
                                        }
                                    }
                                    if !authorized {
                                        return Err(AppError::Forbidden(format!("User is not authorized to update category {}", item.id)));
                                    }
                                }
                            }

                            let next_version = if let Some(row) = record {
                                if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for category {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.id, change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
                            };

                            sqlx::query!(
                                r#"
                                INSERT INTO categories (
                                    id, name, position, "userId", icon, "listId", version, is_deleted, sync_state, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
                                ON CONFLICT (id) DO UPDATE SET
                                    name = EXCLUDED.name,
                                    position = EXCLUDED.position,
                                    "userId" = EXCLUDED."userId",
                                    icon = EXCLUDED.icon,
                                    "listId" = EXCLUDED."listId",
                                    version = EXCLUDED.version,
                                    is_deleted = EXCLUDED.is_deleted,
                                    sync_state = EXCLUDED.sync_state,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                                item.id,
                                item.name,
                                item.position,
                                user_id, // override with authenticated user_id
                                item.icon,
                                item.list_id,
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
                                "Failed to deserialize CategoryData for category {}: {:?}. Data: {:?}",
                                change.id,
                                err,
                                data
                            );
                            return Err(AppError::Serialization(err));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let record = existing_map.get(&change.id);
                    if let Some(row) = record {
                        let mut authorized = row.user_id.as_deref() == Some(user_id);
                        if !authorized {
                            if let Some(ref list_id) = row.list_id {
                                if member_lists_set.contains(list_id) {
                                    authorized = true;
                                }
                            }
                        }
                        if !authorized {
                            return Err(AppError::Forbidden(format!("User is not authorized to update category {}", change.id)));
                        }

                        let next_version = row.version + 1;
                        sqlx::query!(
                            "UPDATE categories SET version = $1, updated_at = $2, updated_by_client = $3, sync_state = 'SYNCED' WHERE id = $4",
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
                if let Some(row) = record {
                    if row.is_deleted {
                        upload_status.push(SuccessResult {
                            id: string_id.clone(),
                            version: row.version,
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(string_id);
                        continue;
                    }

                    let mut authorized = row.user_id.as_deref() == Some(user_id);
                    if !authorized {
                        if let Some(ref list_id) = row.list_id {
                            if member_lists_set.contains(list_id) {
                                authorized = true;
                            }
                        }
                    }
                    if !authorized {
                        return Err(AppError::Forbidden(format!("User is not authorized to delete category {}", change.id)));
                    }
                }

                let row = sqlx::query!(
                    "UPDATE categories SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 WHERE id = $3 RETURNING version",
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
