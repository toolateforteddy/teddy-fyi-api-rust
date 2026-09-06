use super::icons::wants_server_icon;
use crate::routes::sync::batching::RunTracker;
use crate::routes::sync::deletes::ack_unsynced_delete;
use crate::routes::sync::types::*;
use crate::routes::sync::versioning::{advance_version, seed_version};
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use std::collections::HashMap;

/// The kinds of write this processor issues. A run may only contain one of them; see
/// `crate::routes::sync::batching`.
#[derive(PartialEq, Eq)]
enum WriteKind {
    Upsert,
    VersionBump,
    Delete,
}

/// The column vectors for the run currently being accumulated: one `Vec` per column,
/// because that is the shape `UNNEST($1::text[], $2::int4[], ...)` zips back into rows.
/// `updated_by_client` is an array rather than a scalar because the icon assignment below
/// replaces it with `SERVER-AI` for the rows it touched and only those.
#[derive(Default)]
struct Pending {
    up_id: Vec<String>,
    up_title: Vec<String>,
    up_is_completed: Vec<bool>,
    up_created_at: Vec<i64>,
    up_position: Vec<i32>,
    up_scheduled_date: Vec<Option<String>>,
    up_recurrence_rule: Vec<Option<String>>,
    up_scheduled_at: Vec<i64>,
    up_parent_id: Vec<Option<String>>,
    up_is_daily: Vec<bool>,
    up_due_date: Vec<Option<i64>>,
    up_description: Vec<Option<String>>,
    up_list_id: Vec<Option<String>>,
    up_priority: Vec<i32>,
    up_icon: Vec<Option<String>>,
    up_version: Vec<i32>,
    up_is_deleted: Vec<bool>,
    up_updated_by: Vec<String>,

    bump_id: Vec<String>,
    bump_version: Vec<i32>,

    del_id: Vec<String>,
    /// Where in `upload_status` each buffered delete's placeholder sits; the version comes
    /// back from the statement, but the entry has to keep its place in the response.
    del_status_idx: Vec<usize>,
}

impl Pending {
    async fn flush(
        &mut self,
        tx: &mut Transaction<'_, Postgres>,
        user_id: &str,
        client_id: &str,
        server_timestamp: DateTime<Utc>,
        upload_status: &mut [SuccessResult],
    ) -> Result<(), AppError> {
        if !self.up_id.is_empty() {
            sqlx::query!(
                r#"
                INSERT INTO todo_items (
                    id, title, "isCompleted", "createdAt", position, "scheduledDate",
                    "recurrenceRule", "scheduledAt", "userId", "parentId", "isDaily",
                    "dueDate", description, "listId", priority, icon, sync_state, version,
                    is_deleted, updated_at, updated_by_client
                )
                SELECT
                    v.id, v.title, v.is_completed, v.created_at, v.position, v.scheduled_date,
                    v.recurrence_rule, v.scheduled_at, $19, v.parent_id, v.is_daily,
                    v.due_date, v.description, v.list_id, v.priority, v.icon, 'SYNCED',
                    v.version, v.is_deleted, $20, v.updated_by
                FROM UNNEST(
                    $1::text[], $2::text[], $3::bool[], $4::int8[], $5::int4[], $6::text[],
                    $7::text[], $8::int8[], $9::text[], $10::bool[], $11::int8[], $12::text[],
                    $13::text[], $14::int4[], $15::text[], $16::int4[], $17::bool[], $18::text[]
                ) AS v(
                    id, title, is_completed, created_at, position, scheduled_date,
                    recurrence_rule, scheduled_at, parent_id, is_daily, due_date, description,
                    list_id, priority, icon, version, is_deleted, updated_by
                )
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
                &self.up_id,
                &self.up_title,
                &self.up_is_completed,
                &self.up_created_at,
                &self.up_position,
                &self.up_scheduled_date as &[Option<String>],
                &self.up_recurrence_rule as &[Option<String>],
                &self.up_scheduled_at,
                &self.up_parent_id as &[Option<String>],
                &self.up_is_daily,
                &self.up_due_date as &[Option<i64>],
                &self.up_description as &[Option<String>],
                &self.up_list_id as &[Option<String>],
                &self.up_priority,
                &self.up_icon as &[Option<String>],
                &self.up_version,
                &self.up_is_deleted,
                &self.up_updated_by,
                user_id,
                server_timestamp
            )
            .execute(&mut **tx)
            .await?;

            self.up_id.clear();
            self.up_title.clear();
            self.up_is_completed.clear();
            self.up_created_at.clear();
            self.up_position.clear();
            self.up_scheduled_date.clear();
            self.up_recurrence_rule.clear();
            self.up_scheduled_at.clear();
            self.up_parent_id.clear();
            self.up_is_daily.clear();
            self.up_due_date.clear();
            self.up_description.clear();
            self.up_list_id.clear();
            self.up_priority.clear();
            self.up_icon.clear();
            self.up_version.clear();
            self.up_is_deleted.clear();
            self.up_updated_by.clear();
        }

        if !self.bump_id.is_empty() {
            sqlx::query!(
                r#"
                UPDATE todo_items SET
                    version = v.version,
                    updated_at = $3,
                    updated_by_client = $4,
                    sync_state = 'SYNCED'
                FROM UNNEST($1::text[], $2::int4[]) AS v(id, version)
                WHERE todo_items.id = v.id
                "#,
                &self.bump_id,
                &self.bump_version,
                server_timestamp,
                client_id
            )
            .execute(&mut **tx)
            .await?;

            self.bump_id.clear();
            self.bump_version.clear();
        }

        if !self.del_id.is_empty() {
            // `RETURNING id, version` rather than the single-row `RETURNING version` a
            // per-item delete would use: the ids that come back are the rows the
            // server actually had, and the ones that do not are acknowledged as already
            // deleted. See `crate::routes::sync::deletes` for why a delete for a missing
            // row must not fail the batch.
            let updated = sqlx::query!(
                r#"
                UPDATE todo_items SET
                    is_deleted = TRUE,
                    version = version + 1,
                    updated_at = $1,
                    updated_by_client = $2
                WHERE id = ANY($3)
                RETURNING id, version
                "#,
                server_timestamp,
                client_id,
                &self.del_id
            )
            .fetch_all(&mut **tx)
            .await?;

            let deleted: HashMap<String, i32> =
                updated.into_iter().map(|r| (r.id, r.version)).collect();

            for (id, status_idx) in self.del_id.iter().zip(self.del_status_idx.iter()) {
                let version = match deleted.get(id) {
                    Some(version) => *version,
                    None => ack_unsynced_delete("todo item", id),
                };
                upload_status[*status_idx].version = version;
            }

            self.del_id.clear();
            self.del_status_idx.clear();
        }

        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn process_todo_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    // Server-assigned icons, keyed by change id, already resolved by
    // `super::icons::resolve_todo_icons` *before* this transaction was opened.
    //
    // This used to be `state: &AppState`, because the icon was fetched from
    // Gemini right here — an outbound HTTPS round trip made while holding an
    // open transaction and one of the pool's few connections. Nothing about the
    // decision needs the database, so the calls happen up front and this
    // function does database work only. See `super::icons` for the full
    // reasoning.
    resolved_icons: &HashMap<String, String>,
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

    // Writes are buffered into runs of one kind and flushed as a single statement each.
    // Everything above a write -- authorization, version assignment, what goes into the
    // response and in which order -- is unchanged and still decided per item.
    let mut runs: RunTracker<WriteKind> = RunTracker::new();
    let mut pending = Pending::default();

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

                            // Auto-assign icon if missing and fewer than 3 items are being synced in this batch.
                            //
                            // The predicate is re-checked here rather than being
                            // taken on trust from the map: this is the write, so
                            // this is where "when does the server assign an icon"
                            // is decided. The map only ever answers *what* icon,
                            // and an absent entry — no budget, or Gemini errored —
                            // silently leaves the item as the client sent it,
                            // because the icon is a garnish and failing somebody's
                            // whole sync over it would turn a spend limit into
                            // data loss. See `super::icons`.
                            //
                            // `current_updated_by` is per row, not per request,
                            // which is why the batched insert below carries
                            // `updated_by_client` as an array rather than a scalar.
                            if wants_server_icon(&item, changes.len()) {
                                if let Some(icon) = resolved_icons.get(&change.id) {
                                    item.icon = Some(icon.clone());
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

                            if runs.needs_flush(&WriteKind::Upsert, &item.id) {
                                pending
                                    .flush(tx, user_id, client_id, server_timestamp, upload_status)
                                    .await?;
                                runs.clear();
                            }
                            runs.record(WriteKind::Upsert, item.id.clone());

                            pending.up_id.push(item.id);
                            pending.up_title.push(item.title);
                            pending.up_is_completed.push(item.is_completed);
                            pending.up_created_at.push(item.created_at);
                            pending.up_position.push(item.position);
                            pending.up_scheduled_date.push(item.scheduled_date);
                            pending.up_recurrence_rule.push(item.recurrence_rule);
                            pending.up_scheduled_at.push(item.scheduled_at);
                            pending.up_parent_id.push(item.parent_id);
                            pending.up_is_daily.push(item.is_daily);
                            pending.up_due_date.push(item.due_date);
                            pending.up_description.push(item.description);
                            pending.up_list_id.push(item.list_id);
                            pending.up_priority.push(item.priority);
                            pending.up_icon.push(item.icon);
                            pending.up_version.push(next_version);
                            pending.up_is_deleted.push(item.is_deleted);
                            pending.up_updated_by.push(current_updated_by);

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
                            return Err(crate::routes::sync::rejections::item_payload_rejected(
                                "todo item",
                                &change.id.to_string(),
                                &err,
                            ));
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

                        if runs.needs_flush(&WriteKind::VersionBump, &change.id) {
                            pending
                                .flush(tx, user_id, client_id, server_timestamp, upload_status)
                                .await?;
                            runs.clear();
                        }
                        runs.record(WriteKind::VersionBump, change.id.clone());

                        pending.bump_id.push(change.id.clone());
                        pending.bump_version.push(next_version);

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

                // No `else` for the missing row: a delete for a row the server never had
                // is acknowledged, not refused. See `crate::routes::sync::deletes`.
                if runs.needs_flush(&WriteKind::Delete, &change.id) {
                    pending
                        .flush(tx, user_id, client_id, server_timestamp, upload_status)
                        .await?;
                    runs.clear();
                }
                runs.record(WriteKind::Delete, change.id.clone());

                pending.del_id.push(change.id.clone());
                pending.del_status_idx.push(upload_status.len());

                upload_status.push(SuccessResult {
                    id: change.id.clone(),
                    // Patched by the flush that issues this delete, which is what learns
                    // the row's new version.
                    version: 0,
                    sync_state: "SYNCED".to_string(),
                });
                success_ids.push(change.id.clone());
            }
        }
    }

    pending
        .flush(tx, user_id, client_id, server_timestamp, upload_status)
        .await?;

    Ok(())
}
