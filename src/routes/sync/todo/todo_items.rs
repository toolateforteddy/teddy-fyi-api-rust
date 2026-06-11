use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use crate::routes::sync::types::*;

pub async fn process_todo_changes(
    tx: &mut Transaction<'_, Postgres>,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[TodoChangeDelta],
    success_ids: &mut Vec<String>,
) -> Result<(), AppError> {
    for change in changes {
        match change.operation_type {
            OperationType::Insert => {
                tracing::info!("Inserting todo {}", change.id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<TodoItemData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!("SELECT version FROM todo_items WHERE id = $1", change.id)
                                .fetch_optional(&mut **tx)
                                .await?;

                            let next_version = if let Some(row) = record {
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
                            };

                            sqlx::query(
                                r#"
                                INSERT INTO todo_items (
                                    id, title, "isCompleted", "createdAt", position, "scheduledDate",
                                    "recurrenceRule", "scheduledAt", "userId", "parentId", "isDaily",
                                    "dueDate", description, "listId", priority, sync_state, version,
                                    is_deleted, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)
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
                                    version = EXCLUDED.version,
                                    is_deleted = EXCLUDED.is_deleted,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                            )
                            .bind(&item.id)
                            .bind(&item.title)
                            .bind(item.is_completed)
                            .bind(item.created_at)
                            .bind(item.position)
                            .bind(&item.scheduled_date)
                            .bind(&item.recurrence_rule)
                            .bind(item.scheduled_at)
                            .bind(&item.user_id)
                            .bind(&item.parent_id)
                            .bind(item.is_daily)
                            .bind(item.due_date)
                            .bind(&item.description)
                            .bind(&item.list_id)
                            .bind(item.priority)
                            .bind(&item.sync_state)
                            .bind(next_version)
                            .bind(item.is_deleted)
                            .bind(server_timestamp)
                            .bind(client_id)
                            .execute(&mut **tx)
                            .await?;
                        }
                        Err(err) => {
                            tracing::error!("Failed to deserialize TodoItemData for todo {}: {:?}", change.id, err);
                        }
                    }
                }
                success_ids.push(change.id.clone());
            }
            OperationType::Update => {
                tracing::info!("Updating todo {}", change.id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<TodoItemData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!("SELECT version FROM todo_items WHERE id = $1", change.id)
                                .fetch_optional(&mut **tx)
                                .await?;

                            let next_version = if let Some(row) = record {
                                if change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for todo {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.id, change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, change.version) + 1
                            } else {
                                change.version
                            };

                            sqlx::query(
                                r#"
                                INSERT INTO todo_items (
                                    id, title, "isCompleted", "createdAt", position, "scheduledDate",
                                    "recurrenceRule", "scheduledAt", "userId", "parentId", "isDaily",
                                    "dueDate", description, "listId", priority, sync_state, version,
                                    is_deleted, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)
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
                                    version = EXCLUDED.version,
                                    is_deleted = EXCLUDED.is_deleted,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                            )
                            .bind(&item.id)
                            .bind(&item.title)
                            .bind(item.is_completed)
                            .bind(item.created_at)
                            .bind(item.position)
                            .bind(&item.scheduled_date)
                            .bind(&item.recurrence_rule)
                            .bind(item.scheduled_at)
                            .bind(&item.user_id)
                            .bind(&item.parent_id)
                            .bind(item.is_daily)
                            .bind(item.due_date)
                            .bind(&item.description)
                            .bind(&item.list_id)
                            .bind(item.priority)
                            .bind(&item.sync_state)
                            .bind(next_version)
                            .bind(item.is_deleted)
                            .bind(server_timestamp)
                            .bind(client_id)
                            .execute(&mut **tx)
                            .await?;
                        }
                        Err(err) => {
                            tracing::error!("Failed to deserialize TodoItemData for todo {}: {:?}", change.id, err);
                        }
                    }
                } else {
                    let record = sqlx::query!("SELECT version FROM todo_items WHERE id = $1", change.id)
                        .fetch_optional(&mut **tx)
                        .await?;

                    if let Some(row) = record {
                        let next_version = row.version + 1;
                        if change.version < row.version {
                            tracing::warn!(
                                "MVCC Conflict for todo {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                change.id, change.version, row.version
                            );
                        }

                        sqlx::query!(
                            "UPDATE todo_items SET version = $1, updated_at = $2, updated_by_client = $3 WHERE id = $4",
                            next_version,
                            server_timestamp,
                            client_id,
                            change.id
                        )
                        .execute(&mut **tx)
                        .await?;
                    }
                }
                success_ids.push(change.id.clone());
            }
            OperationType::Delete => {
                sqlx::query!(
                    "UPDATE todo_items SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 WHERE id = $3",
                    server_timestamp,
                    client_id,
                    change.id
                )
                .execute(&mut **tx)
                .await?;
                success_ids.push(change.id.clone());
            }
        }
    }
    Ok(())
}
