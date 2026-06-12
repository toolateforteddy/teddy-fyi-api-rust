use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn process_todo_list_changes(
    tx: &mut Transaction<'_, Postgres>,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[TodoListChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
) -> Result<(), AppError> {
    for change in changes {
        match change.operation_type {
            OperationType::Insert => {
                tracing::info!("Inserting todo list {}", change.id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<TodoListData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!(
                                "SELECT version FROM todo_lists WHERE id = $1",
                                change.id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            let next_version = if let Some(row) = record {
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
                            };

                            sqlx::query(
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
                            )
                            .bind(&item.id)
                            .bind(&item.name)
                            .bind(&item.color_hex)
                            .bind(&item.user_id)
                            .bind(item.created_at)
                            .bind("SYNCED")
                            .bind(next_version)
                            .bind(item.is_deleted)
                            .bind(server_timestamp)
                            .bind(client_id)
                            .execute(&mut **tx)
                            .await?;

                            upload_status.push(SuccessResult {
                                id: change.id.clone(),
                                version: next_version,
                                sync_state: "SYNCED".to_string(),
                            });
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize TodoListData for todo list {}: {:?}",
                                change.id,
                                err
                            );
                        }
                    }
                }
                success_ids.push(change.id.clone());
            }
            OperationType::Update => {
                tracing::info!("Updating todo list {}", change.id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<TodoListData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!(
                                "SELECT version FROM todo_lists WHERE id = $1",
                                change.id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            let next_version = if let Some(row) = record {
                                if change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for todo list {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.id, change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, change.version) + 1
                            } else {
                                change.version
                            };

                            sqlx::query(
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
                            )
                            .bind(&item.id)
                            .bind(&item.name)
                            .bind(&item.color_hex)
                            .bind(&item.user_id)
                            .bind(item.created_at)
                            .bind("SYNCED")
                            .bind(next_version)
                            .bind(item.is_deleted)
                            .bind(server_timestamp)
                            .bind(client_id)
                            .execute(&mut **tx)
                            .await?;

                            upload_status.push(SuccessResult {
                                id: change.id.clone(),
                                version: next_version,
                                sync_state: "SYNCED".to_string(),
                            });
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize TodoListData for todo list {}: {:?}",
                                change.id,
                                err
                            );
                        }
                    }
                } else {
                    let record =
                        sqlx::query!("SELECT version FROM todo_lists WHERE id = $1", change.id)
                            .fetch_optional(&mut **tx)
                            .await?;

                    if let Some(row) = record {
                        let next_version = row.version + 1;
                        if change.version < row.version {
                            tracing::warn!(
                                "MVCC Conflict for todo list {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                change.id, change.version, row.version
                            );
                        }

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
                    }
                }
                success_ids.push(change.id.clone());
            }
            OperationType::Delete => {
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
