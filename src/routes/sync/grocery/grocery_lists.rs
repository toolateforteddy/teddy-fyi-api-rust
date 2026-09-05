use crate::routes::sync::batching::RunTracker;
use crate::routes::sync::deletes::ack_unsynced_delete;
use crate::routes::sync::types::*;
use crate::routes::sync::versioning::{advance_version, seed_version};
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};
use std::collections::HashMap;

/// The kinds of write this processor issues. A run may only contain one of them; see
/// `crate::routes::sync::batching`.
///
/// The two deletes are separate kinds because they are different writes: the owner's
/// tears the list and everything hanging off it down, and a member's only ends their own
/// membership.
#[derive(PartialEq, Eq)]
enum WriteKind {
    Upsert,
    VersionBump,
    OwnerDelete,
    LeaveDelete,
}

/// The column vectors for the run currently being accumulated: one `Vec` per column,
/// because that is the shape `UNNEST($1::text[], $2::int4[], ...)` zips back into rows.
#[derive(Default)]
struct Pending {
    up_id: Vec<String>,
    up_name: Vec<String>,
    up_owner_id: Vec<Option<String>>,
    up_created_at: Vec<i64>,
    up_version: Vec<i32>,
    up_is_deleted: Vec<bool>,

    /// The creator's ADMIN membership, for the lists in the run that did not already have
    /// one. Written after the lists themselves, because `"listId"` is a foreign key onto
    /// them.
    mem_id: Vec<String>,
    mem_list_id: Vec<String>,
    mem_joined_at: Vec<i64>,

    bump_id: Vec<String>,
    bump_version: Vec<i32>,

    odel_id: Vec<String>,
    /// Where in `upload_status` each buffered owner-delete's placeholder sits; the version
    /// comes back from the statement, but the entry has to keep its place in the response.
    odel_status_idx: Vec<usize>,

    /// The caller's own membership rows, for lists they are leaving rather than deleting.
    /// No status indices: the version reported for a leave is the list's, which is already
    /// known in the loop.
    ldel_member_id: Vec<String>,
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
                INSERT INTO grocery_lists (
                    id, name, "ownerId", "createdAt", version, is_deleted, sync_state, updated_at, updated_by_client
                )
                SELECT v.id, v.name, v.owner_id, v.created_at, v.version, v.is_deleted,
                       'SYNCED', $7, $8
                FROM UNNEST($1::text[], $2::text[], $3::text[], $4::int8[], $5::int4[], $6::bool[])
                    AS v(id, name, owner_id, created_at, version, is_deleted)
                ON CONFLICT (id) DO UPDATE SET
                    name = EXCLUDED.name,
                    "ownerId" = EXCLUDED."ownerId",
                    version = EXCLUDED.version,
                    is_deleted = EXCLUDED.is_deleted,
                    sync_state = EXCLUDED.sync_state,
                    updated_at = EXCLUDED.updated_at,
                    updated_by_client = EXCLUDED.updated_by_client
                "#,
                &self.up_id,
                &self.up_name,
                &self.up_owner_id as &[Option<String>],
                &self.up_created_at,
                &self.up_version,
                &self.up_is_deleted,
                server_timestamp,
                client_id
            )
            .execute(&mut **tx)
            .await?;

            self.up_id.clear();
            self.up_name.clear();
            self.up_owner_id.clear();
            self.up_created_at.clear();
            self.up_version.clear();
            self.up_is_deleted.clear();
        }

        if !self.mem_id.is_empty() {
            sqlx::query!(
                r#"
                INSERT INTO grocery_list_members (id, "listId", "userId", role, "joinedAt", version, sync_state, updated_at, updated_by_client)
                SELECT v.id, v.list_id, $4, 'ADMIN', v.joined_at, 1, 'SYNCED', $5, NULL
                FROM UNNEST($1::text[], $2::text[], $3::int8[]) AS v(id, list_id, joined_at)
                ON CONFLICT (id) DO NOTHING
                "#,
                &self.mem_id,
                &self.mem_list_id,
                &self.mem_joined_at,
                user_id,
                server_timestamp
            )
            .execute(&mut **tx)
            .await?;

            self.mem_id.clear();
            self.mem_list_id.clear();
            self.mem_joined_at.clear();
        }

        if !self.bump_id.is_empty() {
            sqlx::query!(
                r#"
                UPDATE grocery_lists SET
                    version = v.version,
                    updated_at = $3,
                    updated_by_client = $4,
                    sync_state = 'SYNCED'
                FROM UNNEST($1::text[], $2::int4[]) AS v(id, version)
                WHERE grocery_lists.id = v.id
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

        if !self.odel_id.is_empty() {
            // `RETURNING id, version` rather than the single-row `RETURNING version` a
            // per-item delete would use: the ids that come back are the lists the
            // server actually had, and the ones that do not are acknowledged as already
            // deleted. See `crate::routes::sync::deletes`.
            let updated = sqlx::query!(
                r#"
                UPDATE grocery_lists SET
                    is_deleted = TRUE,
                    version = version + 1,
                    updated_at = $1,
                    updated_by_client = $2
                WHERE id = ANY($3)
                RETURNING id, version
                "#,
                server_timestamp,
                client_id,
                &self.odel_id
            )
            .fetch_all(&mut **tx)
            .await?;

            let deleted: HashMap<String, i32> =
                updated.into_iter().map(|r| (r.id, r.version)).collect();

            // The five statements that tear down what hangs off the list were already
            // set-based per list (`WHERE "listId" = $3`); widening them to `= ANY($3)`
            // makes them set-based per run as well.
            sqlx::query!(
                r#"UPDATE grocery_items
                   SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2
                   WHERE "listId" = ANY($3) AND is_deleted = FALSE"#,
                server_timestamp,
                client_id,
                &self.odel_id
            )
            .execute(&mut **tx)
            .await?;

            sqlx::query!(
                r#"UPDATE grocery_list_members
                   SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2
                   WHERE "listId" = ANY($3) AND is_deleted = FALSE"#,
                server_timestamp,
                client_id,
                &self.odel_id
            )
            .execute(&mut **tx)
            .await?;

            sqlx::query!(
                r#"UPDATE stores
                   SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2
                   WHERE "listId" = ANY($3) AND is_deleted = FALSE"#,
                server_timestamp,
                client_id,
                &self.odel_id
            )
            .execute(&mut **tx)
            .await?;

            sqlx::query!(
                r#"UPDATE categories
                   SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2
                   WHERE "listId" = ANY($3) AND is_deleted = FALSE"#,
                server_timestamp,
                client_id,
                &self.odel_id
            )
            .execute(&mut **tx)
            .await?;

            sqlx::query!(
                r#"DELETE FROM list_invites WHERE "listId" = ANY($1)"#,
                &self.odel_id
            )
            .execute(&mut **tx)
            .await?;

            for (id, status_idx) in self.odel_id.iter().zip(self.odel_status_idx.iter()) {
                let version = match deleted.get(id) {
                    Some(version) => *version,
                    None => ack_unsynced_delete("grocery list", id),
                };
                upload_status[*status_idx].version = version;
            }

            self.odel_id.clear();
            self.odel_status_idx.clear();
        }

        if !self.ldel_member_id.is_empty() {
            sqlx::query!(
                r#"UPDATE grocery_list_members
                   SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2
                   WHERE id = ANY($3)"#,
                server_timestamp,
                client_id,
                &self.ldel_member_id
            )
            .execute(&mut **tx)
            .await?;

            self.ldel_member_id.clear();
        }

        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn process_grocery_list_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[GroceryListChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
    remote_changes: &mut Vec<GroceryListChangeDelta>,
) -> Result<(), AppError> {
    let change_ids: Vec<String> = changes.iter().map(|c| c.id.clone()).collect();
    let existing_records = sqlx::query!(
        r#"SELECT id, name, "ownerId" as owner_id, "createdAt" as created_at, version, is_deleted, sync_state FROM grocery_lists WHERE id = ANY($1)"#,
        &change_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let existing_map: std::collections::HashMap<String, _> = existing_records
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();

    let membership_records = sqlx::query!(
        r#"SELECT id, "listId" as list_id, "userId" as user_id, role, is_deleted FROM grocery_list_members WHERE "userId" = $1 AND "listId" = ANY($2)"#,
        user_id,
        &change_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let membership_map: std::collections::HashMap<String, _> = membership_records
        .into_iter()
        .map(|r| (r.list_id.clone(), r))
        .collect();

    // Writes are buffered into runs of one kind and flushed as a single statement each.
    // Everything above a write -- authorization, version assignment, what goes into the
    // response and in which order -- is unchanged and still decided per item.
    let mut runs: RunTracker<WriteKind> = RunTracker::new();
    let mut pending = Pending::default();

    for change in changes {
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing grocery list {}", change.id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    if let Some(row) = existing_map.get(&change.id) {
                        let is_owner = row.owner_id.as_deref() == Some(user_id);
                        let mut authorized = is_owner;
                        if !authorized {
                            let is_member = membership_map.get(&change.id)
                                .map(|m| !m.is_deleted)
                                .unwrap_or(false);
                            if is_member {
                                authorized = true;
                            }
                        }
                        if !authorized {
                            return Err(AppError::Forbidden(format!("User is not authorized to update grocery list {}", change.id)));
                        }

                        let item_data = GroceryListData {
                            id: change.id.clone(),
                            name: row.name.clone(),
                            owner_id: row.owner_id.clone(),
                            created_at: row.created_at,
                            version: row.version,
                            is_deleted: row.is_deleted,
                            sync_state: row.sync_state.clone(),
                        };
                        let data_val = serde_json::to_value(&item_data)?;
                        remote_changes.push(GroceryListChangeDelta {
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
                    match serde_json::from_value::<GroceryListData>(data.clone()) {
                        Ok(item) => {
                            let record = existing_map.get(&change.id);

                            if record.is_some() && matches!(change.operation_type, OperationType::Update) {
                                // For Update, verify user is a member of the list
                                let is_member = membership_map.get(&change.id)
                                    .map(|m| !m.is_deleted)
                                    .unwrap_or(false);
                                if !is_member {
                                    return Err(AppError::Forbidden(format!("User is not a member of grocery list {}", change.id)));
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
                                        "Conflicting write for grocery list {} (client version {}, server version {}); accepting it as the later arrival",
                                        change.id, change.version, row.version
                                    );
                                }
                                advance_version("Grocery list", &change.id, row.version)?
                            } else {
                                seed_version("Grocery list", &change.id, item.version)?
                            };

                            if runs.needs_flush(&WriteKind::Upsert, &item.id) {
                                pending
                                    .flush(tx, user_id, client_id, server_timestamp, upload_status)
                                    .await?;
                                runs.clear();
                            }
                            runs.record(WriteKind::Upsert, item.id.clone());

                            // Automatically add the creator as an ADMIN member of the list if not already
                            let member_exists = membership_map.contains_key(&item.id);

                            if !member_exists {
                                pending
                                    .mem_id
                                    .push(format!("{}-member-{}", item.id, user_id));
                                pending.mem_list_id.push(item.id.clone());
                                pending.mem_joined_at.push(item.created_at);
                            }

                            pending.up_id.push(item.id);
                            pending.up_name.push(item.name);
                            pending.up_owner_id.push(item.owner_id);
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
                                "Failed to deserialize GroceryListData for grocery list {}: {:?}. Data: {:?}",
                                change.id,
                                err,
                                data
                            );
                            return Err(AppError::Serialization(err));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let is_member = membership_map.get(&change.id)
                        .map(|m| !m.is_deleted)
                        .unwrap_or(false);
                    if !is_member {
                        return Err(AppError::Forbidden(format!("User is not a member of grocery list {}", change.id)));
                    }

                    let record = existing_map.get(&change.id);

                    if let Some(row) = record {
                        // Bounded like every other version bump here; see `crate::routes::sync::versioning`.
                        let next_version = advance_version("Grocery list", &change.id, row.version)?;
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
                let existing_list = existing_map.get(&change.id);

                let list_version = match &existing_list {
                    Some(row) => {
                        if row.is_deleted {
                            upload_status.push(SuccessResult {
                                id: change.id.clone(),
                                version: row.version,
                                sync_state: "SYNCED".to_string(),
                            });
                            success_ids.push(change.id.clone());
                            continue;
                        }
                        row.version
                    }
                    // A list the server never had: acknowledged, not refused. Membership
                    // below is the authorization check, and there is nothing to be a member
                    // of. See `crate::routes::sync::deletes`.
                    None => {
                        upload_status.push(SuccessResult {
                            id: change.id.clone(),
                            version: crate::routes::sync::deletes::ack_unsynced_delete(
                                "grocery list",
                                &change.id,
                            ),
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(change.id.clone());
                        continue;
                    }
                };

                let member_rec = membership_map.get(&change.id);

                let member_row = match member_rec {
                    Some(row) => {
                        if row.is_deleted {
                            upload_status.push(SuccessResult {
                                id: change.id.clone(),
                                version: list_version,
                                sync_state: "SYNCED".to_string(),
                            });
                            success_ids.push(change.id.clone());
                            continue;
                        }
                        row
                    }
                    None => {
                        return Err(AppError::Forbidden(format!("User is not a member of grocery list {}", change.id)));
                    }
                };

                let is_owner = existing_list.and_then(|l| l.owner_id.as_deref()) == Some(user_id)
                    || member_row.role == "OWNER";

                if is_owner {
                    if runs.needs_flush(&WriteKind::OwnerDelete, &change.id) {
                        pending
                            .flush(tx, user_id, client_id, server_timestamp, upload_status)
                            .await?;
                        runs.clear();
                    }
                    runs.record(WriteKind::OwnerDelete, change.id.clone());

                    pending.odel_id.push(change.id.clone());
                    pending.odel_status_idx.push(upload_status.len());

                    upload_status.push(SuccessResult {
                        id: change.id.clone(),
                        // Patched by the flush that issues this delete, which is what
                        // learns the row's new version.
                        version: 0,
                        sync_state: "SYNCED".to_string(),
                    });
                    success_ids.push(change.id.clone());
                } else {
                    // Non-owner member deleting list: only soft-delete their own membership
                    if runs.needs_flush(&WriteKind::LeaveDelete, &member_row.id) {
                        pending
                            .flush(tx, user_id, client_id, server_timestamp, upload_status)
                            .await?;
                        runs.clear();
                    }
                    runs.record(WriteKind::LeaveDelete, member_row.id.clone());

                    pending.ldel_member_id.push(member_row.id.clone());

                    upload_status.push(SuccessResult {
                        id: change.id.clone(),
                        version: list_version,
                        sync_state: "SYNCED".to_string(),
                    });
                    success_ids.push(change.id.clone());
                }
            }
        }
    }

    pending
        .flush(tx, user_id, client_id, server_timestamp, upload_status)
        .await?;

    Ok(())
}
