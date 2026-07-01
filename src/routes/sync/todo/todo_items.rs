use crate::routes::ai::service::assign_todo_icon;
use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn process_todo_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    gemini_api_key: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[TodoChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
    remote_changes: &mut Vec<TodoChangeDelta>,
) -> Result<(), AppError> {
    let change_ids: Vec<String> = changes.iter().map(|c| c.id.clone()).collect();
    let existing_records = sqlx::query!(
        r#"SELECT id, "userId" as user_id, title, "isCompleted" as is_completed, "createdAt" as created_at, position, "scheduledDate" as scheduled_date, "recurrenceRule" as recurrence_rule, "scheduledAt" as scheduled_at, "parentId" as parent_id, "isDaily" as is_daily, "dueDate" as due_date, description, "listId" as list_id, priority, icon, sync_state, version, is_deleted FROM todo_items WHERE id = ANY($1)"#,
        &change_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let existing_map: std::collections::HashMap<String, _> = existing_records
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();

    for change in changes {
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing todo {}", change.id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    if let Some(row) = existing_map.get(&change.id) {
                        if row.user_id.as_deref() != Some(user_id) {
                            return Err(AppError::Forbidden(format!("User is not authorized to update todo item {}", change.id)));
                        }

                        let item_data = TodoItemData {
                            id: change.id.clone(),
                            title: row.title.clone(),
                            is_completed: row.is_completed,
                            created_at: row.created_at,
                            position: row.position,
                            scheduled_date: row.scheduled_date.clone(),
                            recurrence_rule: row.recurrence_rule.clone(),
                            scheduled_at: row.scheduled_at,
                            user_id: row.user_id.clone(),
                            parent_id: row.parent_id.clone(),
                            is_daily: row.is_daily,
                            due_date: row.due_date,
                            description: row.description.clone(),
                            list_id: row.list_id.clone(),
                            priority: row.priority,
                            icon: row.icon.clone(),
                            sync_state: row.sync_state.clone(),
                            version: row.version,
                            is_deleted: row.is_deleted,
                        };
                        let data_val = serde_json::to_value(&item_data)?;
                        remote_changes.push(TodoChangeDelta {
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
                    match serde_json::from_value::<TodoItemData>(data.clone()) {
                        Ok(mut item) => {
                            let mut current_updated_by = client_id.to_string();

                            // Auto-assign icon if missing and fewer than 3 items are being synced in this batch
                            if changes.len() < 3 && item.icon.as_deref().unwrap_or("").is_empty() {
                                if let Ok(icon) = assign_todo_icon(gemini_api_key, &item.title).await {
                                    item.icon = Some(icon);
                                    // Change updated_by_client so it is returned to the caller as a remote mutation
                                    current_updated_by = "SERVER-AI".to_string();
                                }
                            }

                            let record = existing_map.get(&change.id);

                            if let Some(row) = record {
                                if row.user_id.as_deref() != Some(user_id) {
                                    return Err(AppError::Forbidden(format!("User is not authorized to update todo item {}", item.id)));
                                }
                            }

                            let next_version = if let Some(row) = record {
                                if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for todo {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.id, change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
                            };

                            sqlx::query!(
                                r#"
                                INSERT INTO todo_items (
                                    id, title, "isCompleted", "createdAt", position, "scheduledDate",
                                    "recurrenceRule", "scheduledAt", "userId", "parentId", "isDaily",
                                    "dueDate", description, "listId", priority, icon, sync_state, version,
                                    is_deleted, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21)
                                ON CONFLICT (id) DO UPDATE SET
                                    title = EXCLUDED.title,
                                    "isCompleted" = EXCLUDED."isCompleted",
                                    position = EXCLUDED.position,
                                    "scheduledDate" = EXCLUDED."scheduledDate",
                                    "recurrenceRule" = EXCLUDED."recurrenceRule",
                                    "scheduledAt" = EXCLUDED."scheduledAt",
                                    "userId" = EXCLUDED."userId",
                                    "parentId" = EXCLUDED."parentId",
                                    "isDaily" = EXCLUDED."isDaily",
                                    "dueDate" = EXCLUDED."dueDate",
                                    description = EXCLUDED.description,
                                    "listId" = EXCLUDED."listId",
                                    priority = EXCLUDED.priority,
                                    icon = EXCLUDED.icon,
                                    sync_state = EXCLUDED.sync_state,
                                    version = EXCLUDED.version,
                                    is_deleted = EXCLUDED.is_deleted,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                                item.id,
                                item.title,
                                item.is_completed,
                                item.created_at,
                                item.position,
                                item.scheduled_date,
                                item.recurrence_rule,
                                item.scheduled_at,
                                user_id, // override with authenticated user_id
                                item.parent_id,
                                item.is_daily,
                                item.due_date,
                                item.description,
                                item.list_id,
                                item.priority,
                                item.icon,
                                "SYNCED",
                                next_version,
                                item.is_deleted,
                                server_timestamp,
                                current_updated_by
                            )
                            .execute(&mut **tx)
                            .await?;

                            upload_status.push(SuccessResult {
                                id: change.id.clone(),
                                version: next_version,
                                sync_state: "SYNCED".to_string(),
                            });
                            success_ids.push(change.id.clone());
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize TodoItemData for todo {}: {:?}. Data: {:?}",
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
                        if row.user_id.as_deref() != Some(user_id) {
                            return Err(AppError::Forbidden(format!("User is not authorized to update todo item {}", change.id)));
                        }
                        let next_version = row.version + 1;
                        sqlx::query!(
                            "UPDATE todo_items SET version = $1, updated_at = $2, updated_by_client = $3, sync_state = 'SYNCED' WHERE id = $4",
                            next_version,
                            server_timestamp,
                            client_id,
                            change.id
                        )
                        .execute(&mut **tx)
                        .await?;

                        upload_status.push(SuccessResult {
                            id: change.id.clone(),
                            version: next_version,
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(change.id.clone());
                    }
                }
            }
            OperationType::Delete => {
                let record = existing_map.get(&change.id);
                if let Some(row) = record {
                    if row.is_deleted {
                        upload_status.push(SuccessResult {
                            id: change.id.clone(),
                            version: row.version,
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(change.id.clone());
                        continue;
                    }
                    if row.user_id.as_deref() != Some(user_id) {
                        return Err(AppError::Forbidden(format!("User is not authorized to delete todo item {}", change.id)));
                    }
                }

                let row = sqlx::query!(
                    "UPDATE todo_items SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 WHERE id = $3 RETURNING version",
                    server_timestamp,
                    client_id,
                    change.id
                )
                .fetch_one(&mut **tx)
                .await?;

                upload_status.push(SuccessResult {
                    id: change.id.clone(),
                    version: row.version,
                    sync_state: "SYNCED".to_string(),
                });
                success_ids.push(change.id.clone());
            }
        }
    }
    Ok(())
}
