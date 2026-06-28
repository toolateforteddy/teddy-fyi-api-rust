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
    for change in changes {
        let string_id = change.id.clone();
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing category {}", change.id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    let existing = sqlx::query!(
                        r#"SELECT name, position, "userId" as user_id, icon, version, is_deleted, sync_state, "listId" as list_id FROM categories WHERE id = $1"#,
                        change.id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = existing {
                        let mut authorized = row.user_id.as_deref() == Some(user_id);
                        if !authorized {
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
                            }
                        }
                        if !authorized {
                            return Err(AppError::Forbidden(format!("User is not authorized to update category {}", change.id)));
                        }

                        let item_data = CategoryData {
                            id: change.id.clone(),
                            name: row.name,
                            position: row.position,
                            user_id: row.user_id,
                            icon: row.icon,
                            version: row.version,
                            is_deleted: row.is_deleted,
                            sync_state: row.sync_state,
                            list_id: row.list_id,
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
                            let record = sqlx::query!(
                                "SELECT version FROM categories WHERE id = $1",
                                item.id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

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
                                    return Err(AppError::Forbidden(format!("User is not a member of list {}", list_id)));
                                }
                            }

                            if record.is_some() {
                                let existing = sqlx::query!(
                                    r#"SELECT "userId" as user_id, "listId" as list_id FROM categories WHERE id = $1"#,
                                    item.id
                                )
                                .fetch_one(&mut **tx)
                                .await?;
                                let mut authorized = existing.user_id.as_deref() == Some(user_id);
                                if !authorized {
                                    if let Some(ref list_id) = existing.list_id {
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
                                    return Err(AppError::Forbidden(format!("User is not authorized to update category {}", item.id)));
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
                    let existing = sqlx::query!(
                        r#"SELECT "userId" as user_id, "listId" as list_id FROM categories WHERE id = $1"#,
                        change.id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;
                    if let Some(row) = existing {
                        let mut authorized = row.user_id.as_deref() == Some(user_id);
                        if !authorized {
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
                            }
                        }
                        if !authorized {
                            return Err(AppError::Forbidden(format!("User is not authorized to update category {}", change.id)));
                        }
                    }

                    let record =
                        sqlx::query!("SELECT version FROM categories WHERE id = $1", change.id)
                            .fetch_optional(&mut **tx)
                            .await?;

                    if let Some(row) = record {
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
                let existing = sqlx::query!(
                    r#"SELECT "userId" as user_id, "listId" as list_id, is_deleted, version FROM categories WHERE id = $1"#,
                    change.id
                )
                .fetch_optional(&mut **tx)
                .await?;
                if let Some(row) = existing {
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
