use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn process_todo_list_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[TodoListChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
    remote_changes: &mut Vec<TodoListChangeDelta>,
) -> Result<(), AppError> {
    let change_ids: Vec<String> = changes.iter().map(|c| c.id.clone()).collect();
    let existing_records = sqlx::query!(
        r#"SELECT id, "userId" as user_id, name, "colorHex" as color_hex, "createdAt" as created_at, sync_state, version, is_deleted FROM todo_lists WHERE id = ANY($1)"#,
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
                tracing::info!("Processing todo list {}", change.id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    if let Some(row) = existing_map.get(&change.id) {
                        if row.user_id.as_deref() != Some(user_id) {
                            return Err(AppError::Forbidden(format!("User is not authorized to update todo list {}", change.id)));
                        }

                        let item_data = TodoListData {
                            id: change.id.clone(),
                            name: row.name.clone(),
                            color_hex: row.color_hex.clone(),
                            user_id: row.user_id.clone(),
                            created_at: row.created_at,
                            sync_state: row.sync_state.clone(),
                            version: row.version,
                            is_deleted: row.is_deleted,
                        };
                        let data_val = serde_json::to_value(&item_data)?;
                        remote_changes.push(TodoListChangeDelta {
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
                    match serde_json::from_value::<TodoListData>(data.clone()) {
                        Ok(item) => {
                            let record = existing_map.get(&change.id);

                            if let Some(row) = record {
                                if row.user_id.as_deref() != Some(user_id) {
                                    return Err(AppError::Forbidden(format!("User is not authorized to update todo list {}", item.id)));
                                }
                            }

                            let next_version = if let Some(row) = record {
                                if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for todo list {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.id, change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
                            };

                            sqlx::query!(
                                r#"
                                INSERT INTO todo_lists (
                                    id, name, "colorHex", "userId", "createdAt", sync_state, version,
                                    is_deleted, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                                ON CONFLICT (id) DO UPDATE SET
                                    name = EXCLUDED.name,
                                    "colorHex" = EXCLUDED."colorHex",
                                    "userId" = EXCLUDED."userId",
                                    sync_state = EXCLUDED.sync_state,
                                    version = EXCLUDED.version,
                                    is_deleted = EXCLUDED.is_deleted,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                                item.id,
                                item.name,
                                item.color_hex,
                                user_id, // override with authenticated user_id
                                item.created_at,
                                "SYNCED",
                                next_version,
                                item.is_deleted,
                                server_timestamp,
                                client_id
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
                                "Failed to deserialize TodoListData for todo list {}: {:?}. Data: {:?}",
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
                            return Err(AppError::Forbidden(format!("User is not authorized to update todo list {}", change.id)));
                        }
                        let next_version = row.version + 1;
                        sqlx::query!(
                            "UPDATE todo_lists SET version = $1, updated_at = $2, updated_by_client = $3, sync_state = 'SYNCED' WHERE id = $4",
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
                        return Err(AppError::Forbidden(format!("User is not authorized to delete todo list {}", change.id)));
                    }
                }

                let row = sqlx::query!(
                    "UPDATE todo_lists SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 WHERE id = $3 RETURNING version",
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
