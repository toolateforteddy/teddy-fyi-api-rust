use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn fetch_remote_todo_mutations(
    tx: &mut Transaction<'_, Postgres>,
    client_id: &str,
    last_synced_at: Option<DateTime<Utc>>,
) -> Result<(Vec<TodoListChangeDelta>, Vec<TodoChangeDelta>), AppError> {
    let mut remote_todo_list_changes = Vec::new();
    let mut remote_todo_changes = Vec::new();

    if let Some(last_synced_at) = last_synced_at {
        // Fetch todo_lists changed after last_synced_at by OTHER clients
        let updated_todo_lists = sqlx::query!(
            r#"SELECT
                id, name, "colorHex" as color_hex, "userId" as user_id, "createdAt" as created_at, sync_state, version, is_deleted
               FROM todo_lists
               WHERE updated_at > $1 AND (updated_by_client != $2 OR updated_by_client IS NULL)"#,
            last_synced_at,
            client_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in updated_todo_lists {
            let item_data = TodoListData {
                id: row.id.clone(),
                name: row.name,
                color_hex: row.color_hex,
                user_id: row.user_id,
                created_at: row.created_at,
                sync_state: row.sync_state,
                version: row.version,
                is_deleted: row.is_deleted,
            };

            let data_val = serde_json::to_value(&item_data).ok();

            remote_todo_list_changes.push(TodoListChangeDelta {
                id: row.id,
                operation_type: if row.is_deleted {
                    OperationType::Delete
                } else {
                    OperationType::Update
                },
                version: row.version,
                data: data_val,
            });
        }

        // Fetch todo_items changed after last_synced_at by OTHER clients
        let updated_todos = sqlx::query!(
            r#"SELECT
                id, title, "isCompleted" as is_completed, "createdAt" as created_at, position, "scheduledDate" as scheduled_date,
                "recurrenceRule" as recurrence_rule, "scheduledAt" as scheduled_at, "userId" as user_id, "parentId" as parent_id, "isDaily" as is_daily,
                "dueDate" as due_date, description, "listId" as list_id, priority, icon, sync_state, version, is_deleted
               FROM todo_items
               WHERE updated_at > $1 AND (updated_by_client != $2 OR updated_by_client IS NULL)"#,
            last_synced_at,
            client_id
        )
        .fetch_all(&mut **tx)
        .await?;

        for row in updated_todos {
            let item_data = TodoItemData {
                id: row.id.clone(),
                title: row.title,
                is_completed: row.is_completed,
                created_at: row.created_at,
                position: row.position,
                scheduled_date: row.scheduled_date,
                recurrence_rule: row.recurrence_rule,
                scheduled_at: row.scheduled_at,
                user_id: row.user_id,
                parent_id: row.parent_id,
                is_daily: row.is_daily,
                due_date: row.due_date,
                description: row.description,
                list_id: row.list_id,
                priority: row.priority,
                icon: row.icon,
                sync_state: row.sync_state,
                version: row.version,
                is_deleted: row.is_deleted,
            };

            let data_val = serde_json::to_value(&item_data).ok();

            remote_todo_changes.push(TodoChangeDelta {
                id: row.id,
                operation_type: if row.is_deleted {
                    OperationType::Delete
                } else {
                    OperationType::Update
                },
                version: row.version,
                data: data_val,
            });
        }
    }

    Ok((remote_todo_list_changes, remote_todo_changes))
}
