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
) -> Result<(), AppError> {
    for change in changes {
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing todo {}", change.id);
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

                            let record = sqlx::query!(
                                "SELECT version FROM todo_items WHERE id = $1",
                                change.id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            if record.is_some() {
                                let owner = sqlx::query!(
                                    r#"SELECT "userId" as user_id FROM todo_items WHERE id = $1"#,
                                    item.id
                                )
                                .fetch_one(&mut **tx)
                                .await?;
                                if owner.user_id.as_deref() != Some(user_id) {
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
                    let owner = sqlx::query!(
                        r#"SELECT "userId" as user_id FROM todo_items WHERE id = $1"#,
                        change.id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;
                    if let Some(row) = owner {
                        if row.user_id.as_deref() != Some(user_id) {
                            return Err(AppError::Forbidden(format!("User is not authorized to update todo item {}", change.id)));
                        }
                    }

                    let record =
                        sqlx::query!("SELECT version FROM todo_items WHERE id = $1", change.id)
                            .fetch_optional(&mut **tx)
                            .await?;

                    if let Some(row) = record {
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
                let owner = sqlx::query!(
                    r#"SELECT "userId" as user_id FROM todo_items WHERE id = $1"#,
                    change.id
                )
                .fetch_optional(&mut **tx)
                .await?;
                if let Some(row) = owner {
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
