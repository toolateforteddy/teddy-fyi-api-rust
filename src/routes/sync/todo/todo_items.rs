use crate::routes::ai::budget::{charge_gemini_call, BudgetLimits};
use crate::routes::ai::service::assign_todo_icon;
use crate::state::AppState;
use crate::routes::sync::types::*;
use crate::routes::sync::versioning::{advance_version, seed_version};
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

#[allow(clippy::too_many_arguments)]
pub async fn process_todo_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    // Was `gemini_api_key: &str`. Carries the credential, the shared outbound
    // HTTP client and Redis, because this function makes a billed AI call of its
    // own (the icon assignment below) and so needs the same three things
    // `/api/assign-icon` needs.
    state: &AppState,
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
                                // This is the third path that spends the Gemini
                                // budget, and the least obvious one — a client
                                // that never calls `/api/assign-icon` can still
                                // bill us by syncing icon-less todos in batches
                                // of one or two. It therefore charges the same
                                // per-account budget as the explicit endpoints.
                                //
                                // A refusal is swallowed rather than propagated:
                                // the icon is a garnish, and failing somebody's
                                // whole sync because their AI allowance ran out
                                // would turn a spend limit into data loss.
                                let charged = charge_gemini_call(
                                    &state.redis_client,
                                    BudgetLimits::cached(),
                                    user_id,
                                )
                                .await
                                .is_ok();
                                if charged {
                                    if let Ok(icon) = assign_todo_icon(
                                        &state.http_client,
                                        &state.gemini_api_key,
                                        &item.title,
                                    )
                                    .await
                                    {
                                        item.icon = Some(icon);
                                        // Change updated_by_client so it is returned to the caller as a remote mutation
                                        current_updated_by = "SERVER-AI".to_string();
                                    }
                                }
                            }

                            let record = existing_map.get(&change.id);

                            if let Some(row) = record {
                                if row.user_id.as_deref() != Some(user_id) {
                                    return Err(AppError::Forbidden(format!("User is not authorized to update todo item {}", item.id)));
                                }
                            }

                            // One policy for every synced row: the server's stored version is the only
                            // input to the next one, and a row the server has never seen takes a bounded
                            // seed. `max(row.version, item.version) + 1` let a single request carrying an
                            // enormous `version` move this row's counter there permanently -- for a shared
                            // list, for every member of it. See `crate::routes::sync::versioning`.
                            let next_version = if let Some(row) = record {
                                if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                    tracing::warn!(
                                        "Conflicting write for todo {} (client version {}, server version {}); accepting it as the later arrival",
                                        change.id, change.version, row.version
                                    );
                                }
                                advance_version("Todo", &change.id, row.version)?
                            } else {
                                seed_version("Todo", &change.id, item.version)?
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
                        // Bounded like every other version bump here; see `crate::routes::sync::versioning`.
                        let next_version = advance_version("Todo", &change.id, row.version)?;
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
