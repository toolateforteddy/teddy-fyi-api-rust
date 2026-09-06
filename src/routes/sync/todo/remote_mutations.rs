//! The download half of a todo sync, one page at a time.
//!
//! See `crate::routes::sync::paging` for why a download needs a bound and why the page
//! boundary is a whole instant rather than a row. The two tables here are paged
//! independently and the caller takes the earlier of their two cursors, because one
//! `server_timestamp` serves the whole reply: rows past that point are simply re-read on
//! the next sync, which the protocol is idempotent under.

use crate::routes::sync::paging::{probe_limit, trim_page_at, trim_size, PageAt};
use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

/// One page of the todo rows a client is owed.
pub struct TodoDownload {
    pub list_changes: Vec<TodoListChangeDelta>,
    pub changes: Vec<TodoChangeDelta>,
    /// `Some` when either table's page was cut short — see `crate::routes::sync::paging`.
    pub next_cursor: Option<DateTime<Utc>>,
}

pub async fn fetch_remote_todo_mutations(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    last_synced_at: Option<DateTime<Utc>>,
    // `None` serves the whole download in one reply, for a client that cannot resume a
    // truncated one. See `SyncRequest::supports_paging`.
    page_size: Option<usize>,
) -> Result<TodoDownload, AppError> {
    let is_initial_sync = last_synced_at.is_none() || last_synced_at.map(|t| t.timestamp() <= 0).unwrap_or(true);
    let after = last_synced_at.unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap());

    let limit = probe_limit(page_size);
    let trim = trim_size(page_size);

    // ---- todo_lists ----
    let mut list_rows =
        fetch_todo_list_page(tx, user_id, client_id, after, None, is_initial_sync, limit).await?;
    let list_cursor = match trim_page_at(&mut list_rows, trim, |row| row.updated_at) {
        PageAt::Complete => None,
        PageAt::Truncated { next_cursor } => Some(next_cursor),
        PageAt::WholeInstant { at } => {
            tracing::warn!(
                "More than a page of todo lists share updated_at {}; serving that instant whole",
                at
            );
            list_rows =
                fetch_todo_list_page(
                tx,
                user_id,
                client_id,
                // `after` is a strict `>`, so it has to sit one tick below the instant
                // being served or the re-read matches nothing. A microsecond is
                // `timestamptz`'s own resolution, so this cannot skip a row between.
                at - chrono::Duration::microseconds(1),
                Some(at),
                is_initial_sync,
                i64::MAX,
            )
            .await?;
            Some(at)
        }
    };

    let mut list_changes = Vec::with_capacity(list_rows.len());
    for row in list_rows {
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
        let data_val = serde_json::to_value(&item_data)?;
        list_changes.push(TodoListChangeDelta {
            id: row.id,
            operation_type: if row.is_deleted { OperationType::Delete } else { OperationType::Update },
            version: row.version,
            data: Some(data_val),
        });
    }

    // ---- todo_items ----
    let mut item_rows =
        fetch_todo_item_page(tx, user_id, client_id, after, None, is_initial_sync, limit).await?;
    let item_cursor = match trim_page_at(&mut item_rows, trim, |row| row.updated_at) {
        PageAt::Complete => None,
        PageAt::Truncated { next_cursor } => Some(next_cursor),
        PageAt::WholeInstant { at } => {
            tracing::warn!(
                "More than a page of todo items share updated_at {}; serving that instant whole",
                at
            );
            item_rows =
                fetch_todo_item_page(
                tx,
                user_id,
                client_id,
                // `after` is a strict `>`, so it has to sit one tick below the instant
                // being served or the re-read matches nothing. A microsecond is
                // `timestamptz`'s own resolution, so this cannot skip a row between.
                at - chrono::Duration::microseconds(1),
                Some(at),
                is_initial_sync,
                i64::MAX,
            )
            .await?;
            Some(at)
        }
    };

    let mut changes = Vec::with_capacity(item_rows.len());
    for row in item_rows {
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
        let data_val = serde_json::to_value(&item_data)?;
        changes.push(TodoChangeDelta {
            id: row.id,
            operation_type: if row.is_deleted { OperationType::Delete } else { OperationType::Update },
            version: row.version,
            data: Some(data_val),
        });
    }

    Ok(TodoDownload {
        list_changes,
        changes,
        next_cursor: [list_cursor, item_cursor].into_iter().flatten().min(),
    })
}

struct TodoListRow {
    id: String,
    name: String,
    color_hex: String,
    user_id: Option<String>,
    created_at: i64,
    sync_state: String,
    version: i32,
    is_deleted: bool,
    updated_at: DateTime<Utc>,
}

#[allow(clippy::too_many_arguments)]
async fn fetch_todo_list_page(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    after: DateTime<Utc>,
    // Set only on the re-read that serves one over-full instant whole.
    through: Option<DateTime<Utc>>,
    is_initial_sync: bool,
    limit: i64,
) -> Result<Vec<TodoListRow>, AppError> {
    let rows = sqlx::query_as!(
        TodoListRow,
        r#"SELECT
            id, name, "colorHex" as color_hex, "userId" as user_id, "createdAt" as created_at,
            sync_state, version, is_deleted, updated_at
           FROM todo_lists
           WHERE "userId" = $1
             AND updated_at > $2 AND ($4 OR updated_by_client != $3 OR updated_by_client IS NULL)
             AND ($4 = FALSE OR is_deleted = FALSE)
             AND ($5::timestamptz IS NULL OR updated_at <= $5)
           ORDER BY updated_at ASC, id ASC
           LIMIT $6"#,
        user_id,
        after,
        client_id,
        is_initial_sync,
        through,
        limit
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows)
}

struct TodoItemRow {
    id: String,
    title: String,
    is_completed: bool,
    created_at: i64,
    position: i32,
    scheduled_date: Option<String>,
    recurrence_rule: Option<String>,
    scheduled_at: i64,
    user_id: Option<String>,
    parent_id: Option<String>,
    is_daily: bool,
    due_date: Option<i64>,
    description: Option<String>,
    list_id: Option<String>,
    priority: i32,
    icon: Option<String>,
    sync_state: String,
    version: i32,
    is_deleted: bool,
    updated_at: DateTime<Utc>,
}

#[allow(clippy::too_many_arguments)]
async fn fetch_todo_item_page(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    after: DateTime<Utc>,
    through: Option<DateTime<Utc>>,
    is_initial_sync: bool,
    limit: i64,
) -> Result<Vec<TodoItemRow>, AppError> {
    let rows = sqlx::query_as!(
        TodoItemRow,
        r#"SELECT
            id, title, "isCompleted" as is_completed, "createdAt" as created_at, position,
            "scheduledDate" as scheduled_date, "recurrenceRule" as recurrence_rule,
            "scheduledAt" as scheduled_at, "userId" as user_id, "parentId" as parent_id,
            "isDaily" as is_daily, "dueDate" as due_date, description, "listId" as list_id,
            priority, icon, sync_state, version, is_deleted, updated_at
           FROM todo_items
           WHERE "userId" = $1
             AND updated_at > $2 AND ($4 OR updated_by_client != $3 OR updated_by_client IS NULL)
             AND ($4 = FALSE OR is_deleted = FALSE)
             AND ($5::timestamptz IS NULL OR updated_at <= $5)
           ORDER BY updated_at ASC, id ASC
           LIMIT $6"#,
        user_id,
        after,
        client_id,
        is_initial_sync,
        through,
        limit
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(rows)
}
