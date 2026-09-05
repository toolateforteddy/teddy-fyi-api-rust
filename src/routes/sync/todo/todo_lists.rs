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
/// Parameters that are the same for every row in the request -- the authenticated user,
/// the server timestamp, the client -- stay scalars.
#[derive(Default)]
struct Pending {
    up_id: Vec<String>,
    up_name: Vec<String>,
    up_color_hex: Vec<String>,
    up_created_at: Vec<i64>,
    up_version: Vec<i32>,
    up_is_deleted: Vec<bool>,

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
                INSERT INTO todo_lists (
                    id, name, "colorHex", "userId", "createdAt", sync_state, version,
                    is_deleted, updated_at, updated_by_client
                )
                SELECT v.id, v.name, v.color_hex, $7, v.created_at, 'SYNCED', v.version,
                       v.is_deleted, $8, $9
                FROM UNNEST($1::text[], $2::text[], $3::text[], $4::int8[], $5::int4[], $6::bool[])
                    AS v(id, name, color_hex, created_at, version, is_deleted)
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
                &self.up_id,
                &self.up_name,
                &self.up_color_hex,
                &self.up_created_at,
                &self.up_version,
                &self.up_is_deleted,
                user_id,
                server_timestamp,
                client_id
            )
            .execute(&mut **tx)
            .await?;

            self.up_id.clear();
            self.up_name.clear();
            self.up_color_hex.clear();
            self.up_created_at.clear();
            self.up_version.clear();
            self.up_is_deleted.clear();
        }

        if !self.bump_id.is_empty() {
            sqlx::query!(
                r#"
                UPDATE todo_lists SET
                    version = v.version,
                    updated_at = $3,
                    updated_by_client = $4,
                    sync_state = 'SYNCED'
                FROM UNNEST($1::text[], $2::int4[]) AS v(id, version)
                WHERE todo_lists.id = v.id
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
            // `RETURNING id, version` rather than the single-row `RETURNING version` the
            // `soft_delete_version!` macro wraps: the ids that come back are the rows the
            // server actually had, and the ones that do not are acknowledged as already
            // deleted. See `crate::routes::sync::deletes`.
            let updated = sqlx::query!(
                r#"
                UPDATE todo_lists SET
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
                    None => ack_unsynced_delete("todo list", id),
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

    // Writes are buffered into runs of one kind and flushed as a single statement each.
    // Everything above a write -- authorization, version assignment, what goes into the
    // response and in which order -- is unchanged and still decided per item.
    let mut runs: RunTracker<WriteKind> = RunTracker::new();
    let mut pending = Pending::default();

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

                            // One policy for every synced row: the server's stored version is the only
                            // input to the next one, and a row the server has never seen takes a bounded
                            // seed. `max(row.version, item.version) + 1` let a single request carrying an
                            // enormous `version` move this row's counter there permanently -- for a shared
                            // list, for every member of it. See `crate::routes::sync::versioning`.
                            let next_version = if let Some(row) = record {
                                if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                    tracing::warn!(
                                        "Conflicting write for todo list {} (client version {}, server version {}); accepting it as the later arrival",
                                        change.id, change.version, row.version
                                    );
                                }
                                advance_version("Todo list", &change.id, row.version)?
                            } else {
                                seed_version("Todo list", &change.id, item.version)?
                            };

                            if runs.needs_flush(&WriteKind::Upsert, &item.id) {
                                pending
                                    .flush(tx, user_id, client_id, server_timestamp, upload_status)
                                    .await?;
                                runs.clear();
                            }
                            runs.record(WriteKind::Upsert, item.id.clone());

                            // `"userId"` is still the authenticated user, not the one on
                            // the wire; it is the same for every row in the request, so it
                            // stays a scalar in the batched statement.
                            pending.up_id.push(item.id);
                            pending.up_name.push(item.name);
                            pending.up_color_hex.push(item.color_hex);
                            pending.up_created_at.push(item.created_at);
                            pending.up_version.push(next_version);
                            pending.up_is_deleted.push(item.is_deleted);

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
                        // Bounded like every other version bump here; see `crate::routes::sync::versioning`.
                        let next_version = advance_version("Todo list", &change.id, row.version)?;
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
                        return Err(AppError::Forbidden(format!("User is not authorized to delete todo list {}", change.id)));
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
