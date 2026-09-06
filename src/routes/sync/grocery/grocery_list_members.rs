use crate::routes::sync::batching::RunTracker;
use crate::routes::sync::deletes::ack_unsynced_delete;
use crate::routes::sync::types::*;
use crate::routes::sync::versioning::advance_version;
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
        client_id: &str,
        server_timestamp: DateTime<Utc>,
        upload_status: &mut [SuccessResult],
    ) -> Result<(), AppError> {
        if !self.up_id.is_empty() {
            // `"listId"`, `"userId"`, `role` and `"joinedAt"` are absent from this
            // statement on purpose: they are the server's, and a sync payload has no say
            // in them.
            sqlx::query!(
                r#"
                UPDATE grocery_list_members SET
                    version = v.version,
                    is_deleted = v.is_deleted,
                    sync_state = 'SYNCED',
                    updated_at = $4,
                    updated_by_client = $5
                FROM UNNEST($1::text[], $2::int4[], $3::bool[]) AS v(id, version, is_deleted)
                WHERE grocery_list_members.id = v.id
                "#,
                &self.up_id,
                &self.up_version,
                &self.up_is_deleted,
                server_timestamp,
                client_id
            )
            .execute(&mut **tx)
            .await?;

            self.up_id.clear();
            self.up_version.clear();
            self.up_is_deleted.clear();
        }

        if !self.bump_id.is_empty() {
            sqlx::query!(
                r#"
                UPDATE grocery_list_members SET
                    version = v.version,
                    updated_at = $3,
                    updated_by_client = $4,
                    sync_state = 'SYNCED'
                FROM UNNEST($1::text[], $2::int4[]) AS v(id, version)
                WHERE grocery_list_members.id = v.id
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
            // deleted. See `crate::routes::sync::deletes`.
            let updated = sqlx::query!(
                r#"
                UPDATE grocery_list_members SET
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
                    None => ack_unsynced_delete("grocery list membership", id),
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
pub async fn process_grocery_list_member_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[GroceryListMemberChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
    remote_changes: &mut Vec<GroceryListMemberChangeDelta>,
) -> Result<(), AppError> {
    let change_ids: Vec<String> = changes.iter().map(|c| c.id.clone()).collect();
    let existing_records = sqlx::query!(
        r#"SELECT id, "listId" as list_id, "userId" as user_id, role, "joinedAt" as joined_at, version, is_deleted, sync_state FROM grocery_list_members WHERE id = ANY($1)"#,
        &change_ids
    )
    .fetch_all(&mut **tx)
    .await?;

    let existing_map: std::collections::HashMap<String, _> = existing_records
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();

    let mut list_ids = std::collections::HashSet::new();
    // Writes are buffered into runs of one kind and flushed as a single statement each.
    // Everything above a write -- authorization, version assignment, what goes into the
    // response and in which order -- is unchanged and still decided per item.
    let mut runs: RunTracker<WriteKind> = RunTracker::new();
    let mut pending = Pending::default();

    for change in changes {
        if let Some(ref data) = change.data {
            if let Ok(item) = serde_json::from_value::<GroceryListMemberData>(data.clone()) {
                list_ids.insert(item.list_id);
            }
        }
        if let Some(row) = existing_map.get(&change.id) {
            list_ids.insert(row.list_id.clone());
        }
    }
    let list_ids_vec: Vec<String> = list_ids.into_iter().collect();

    // The caller's own live membership rows, in full: the list ids are the permission
    // check, and the rows themselves are what gets echoed back when a client has invented
    // a local row for a membership the server has already granted.
    let membership_records = sqlx::query!(
        r#"SELECT id, "listId" as list_id, "userId" as user_id, role, "joinedAt" as joined_at, version, is_deleted, sync_state FROM grocery_list_members WHERE "userId" = $1 AND "listId" = ANY($2) AND is_deleted = FALSE"#,
        user_id,
        &list_ids_vec
    )
    .fetch_all(&mut **tx)
    .await?;

    let member_lists_set: std::collections::HashSet<String> = membership_records
        .iter()
        .map(|r| r.list_id.clone())
        .collect();

    let own_membership_by_list: std::collections::HashMap<String, _> = membership_records
        .into_iter()
        .map(|r| (r.list_id.clone(), r))
        .collect();

    for change in changes {
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing grocery list member {}", change.id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    if let Some(row) = existing_map.get(&change.id) {
                        let is_self = row.user_id == user_id;
                        let mut authorized = is_self;
                        if !authorized {
                            let is_member = member_lists_set.contains(&row.list_id);
                            if is_member {
                                authorized = true;
                            }
                        }
                        if !authorized {
                            return Err(AppError::Forbidden(format!(
                                "User is not authorized to update membership {}",
                                change.id
                            )));
                        }

                        let item_data = GroceryListMemberData {
                            id: change.id.clone(),
                            list_id: row.list_id.clone(),
                            user_id: row.user_id.clone(),
                            role: row.role.clone(),
                            joined_at: row.joined_at,
                            version: row.version,
                            is_deleted: row.is_deleted,
                            sync_state: row.sync_state.clone(),
                        };
                        let data_val = serde_json::to_value(&item_data)?;
                        remote_changes.push(GroceryListMemberChangeDelta {
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
                    match serde_json::from_value::<GroceryListMemberData>(data.clone()) {
                        Ok(item) => {
                            // Membership is granted by `/api/lists/join` and by nothing
                            // else; sync only ever *reflects* a membership that already
                            // exists. So this path never creates a row, and it never takes
                            // `"listId"`, `"userId"` or `role` from the payload.
                            //
                            // Without that, the whole invite mechanism was optional: a
                            // member of list L could sync an insert with a fresh id,
                            // `listId = L` and `userId` set to anybody, and that account
                            // was a member -- no code, no TTL, no attempt limit. And with
                            // `role = EXCLUDED.role` a member could name themselves OWNER,
                            // which `grocery_lists` reads to authorise deleting the list
                            // and everything on it.
                            let Some(row) = existing_map.get(&change.id) else {
                                // A client that just created a list offline sends the list
                                // and a membership row of its own invention in one batch.
                                // The list processor runs first in this transaction and
                                // seeds the creator's row, so the membership already
                                // exists under the server's own id: accept the change as a
                                // no-op and hand the client the canonical row to replace
                                // its local one with. Anything else is a grant, and is
                                // refused.
                                if item.user_id == user_id {
                                    if let Some(existing) = own_membership_by_list.get(&item.list_id) {
                                        let item_data = GroceryListMemberData {
                                            id: existing.id.clone(),
                                            list_id: existing.list_id.clone(),
                                            user_id: existing.user_id.clone(),
                                            role: existing.role.clone(),
                                            joined_at: existing.joined_at,
                                            version: existing.version,
                                            is_deleted: existing.is_deleted,
                                            sync_state: existing.sync_state.clone(),
                                        };
                                        remote_changes.push(GroceryListMemberChangeDelta {
                                            id: existing.id.clone(),
                                            operation_type: OperationType::Update,
                                            version: existing.version,
                                            data: Some(serde_json::to_value(&item_data)?),
                                        });
                                        success_ids.push(change.id.clone());
                                        continue;
                                    }
                                }

                                return Err(AppError::Forbidden(format!(
                                    "Membership is granted by joining a list with an invite code; sync cannot create membership {}",
                                    change.id
                                )));
                            };

                            // The row's own list is the one that matters -- the payload's
                            // is not trusted to name it. A member of list B could otherwise
                            // take a membership row belonging to list A and move it, either
                            // dragging A's row into B or pushing a co-member out of B.
                            let is_self = row.user_id == user_id;
                            if !is_self && !member_lists_set.contains(&row.list_id) {
                                return Err(AppError::Forbidden(format!(
                                    "User is not authorized to manage membership {}",
                                    change.id
                                )));
                            }

                            // Identity is server-owned. A payload that disagrees about who
                            // this row is, or which list it belongs to, is not a change to
                            // apply -- it is a rewrite of a membership, which only
                            // `/api/lists/join` may do.
                            if item.list_id != row.list_id || item.user_id != row.user_id {
                                return Err(AppError::Forbidden(format!(
                                    "User is not authorized to reassign membership {}",
                                    change.id
                                )));
                            }

                            // Un-deleting is granting: a membership that was given up (or
                            // taken away) comes back through `/api/lists/join`, not by a
                            // client syncing `isDeleted: false` over the top of it.
                            if row.is_deleted && !item.is_deleted {
                                return Err(AppError::Forbidden(format!(
                                    "Membership {} can only be restored by joining the list again",
                                    change.id
                                )));
                            }

                            // One policy for every synced row: the server's stored version is the only
                            // input to the next one. `max(row.version, item.version) + 1` let a single
                            // request carrying an enormous `version` move this row's counter there
                            // permanently -- for a shared list, for every member of it. See
                            // `crate::routes::sync::versioning`.
                            if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                tracing::warn!(
                                    "Conflicting write for member {} (client version {}, server version {}); accepting it as the later arrival",
                                    change.id, change.version, row.version
                                );
                            }
                            let next_version = advance_version("Member", &change.id, row.version)?;

                            if runs.needs_flush(&WriteKind::Upsert, &change.id) {
                                pending
                                    .flush(tx, client_id, server_timestamp, upload_status)
                                    .await?;
                                runs.clear();
                            }
                            runs.record(WriteKind::Upsert, change.id.clone());

                            pending.up_id.push(change.id.clone());
                            pending.up_version.push(next_version);
                            pending.up_is_deleted.push(item.is_deleted);

                            // The client sent a role we ignored, so tell it what the row
                            // actually says rather than leaving the two disagreeing.
                            if item.role != row.role {
                                let item_data = GroceryListMemberData {
                                    id: row.id.clone(),
                                    list_id: row.list_id.clone(),
                                    user_id: row.user_id.clone(),
                                    role: row.role.clone(),
                                    joined_at: row.joined_at,
                                    version: next_version,
                                    is_deleted: item.is_deleted,
                                    sync_state: "SYNCED".to_string(),
                                };
                                remote_changes.push(GroceryListMemberChangeDelta {
                                    id: row.id.clone(),
                                    operation_type: OperationType::Update,
                                    version: next_version,
                                    data: Some(serde_json::to_value(&item_data)?),
                                });
                            }

                            upload_status.push(SuccessResult {
                                id: change.id.clone(),
                                version: next_version,
                                sync_state: "SYNCED".to_string(),
                            });
                            success_ids.push(change.id.clone());
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize GroceryListMemberData for member {}: {:?}. Data: {:?}",
                                change.id,
                                err,
                                data
                            );
                            return Err(crate::routes::sync::rejections::item_payload_rejected(
                                "grocery list member",
                                &change.id.to_string(),
                                &err,
                            ));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let record = existing_map.get(&change.id);
                    if let Some(row) = record {
                        let is_self = row.user_id == user_id;
                        let is_member = member_lists_set.contains(&row.list_id);

                        if !is_self && !is_member {
                            return Err(AppError::Forbidden(format!(
                                "User is not authorized to update membership {}",
                                change.id
                            )));
                        }

                        // Bounded like every other version bump here; see `crate::routes::sync::versioning`.
                        let next_version = advance_version("Member", &change.id, row.version)?;
                        if runs.needs_flush(&WriteKind::VersionBump, &change.id) {
                            pending
                                .flush(tx, client_id, server_timestamp, upload_status)
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
                let member_rec = existing_map.get(&change.id);

                if let Some(row) = member_rec {
                    if row.is_deleted {
                        upload_status.push(SuccessResult {
                            id: change.id.clone(),
                            version: row.version,
                            sync_state: "SYNCED".to_string(),
                        });
                        success_ids.push(change.id.clone());
                        continue;
                    }

                    let is_self = row.user_id == user_id;
                    let is_member = member_lists_set.contains(&row.list_id);

                    if !is_self && !is_member {
                        return Err(AppError::Forbidden(format!(
                            "User is not authorized to delete membership {}",
                            change.id
                        )));
                    }
                }

                // No `else` for the missing row: a delete for a row the server never had
                // is acknowledged, not refused. See `crate::routes::sync::deletes`.
                if runs.needs_flush(&WriteKind::Delete, &change.id) {
                    pending
                        .flush(tx, client_id, server_timestamp, upload_status)
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
        .flush(tx, client_id, server_timestamp, upload_status)
        .await?;

    Ok(())
}
