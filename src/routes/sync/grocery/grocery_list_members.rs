use crate::routes::sync::types::*;
use crate::routes::sync::versioning::{advance_version, seed_version};
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

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

    let membership_records = sqlx::query!(
        r#"SELECT "listId" as list_id FROM grocery_list_members WHERE "userId" = $1 AND "listId" = ANY($2) AND is_deleted = FALSE"#,
        user_id,
        &list_ids_vec
    )
    .fetch_all(&mut **tx)
    .await?;

    let member_lists_set: std::collections::HashSet<String> = membership_records
        .into_iter()
        .map(|r| r.list_id)
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
                            // Verify permission: the caller must already be a member of the
                            // list this row belongs to. Both ends of it, when the row exists.
                            //
                            // Joining *yourself* used to be sufficient on its own, and it is
                            // not a permission check at all: a listId is not a secret — it
                            // rides along in every grocery item, store and category the list
                            // owns — so knowing one was enough to insert a MEMBER row against
                            // another family's list and start reading and writing their data.
                            // No guessing required. Membership is granted by `/api/lists/join`
                            // and by nothing else; sync only ever reflects a membership that
                            // already exists.
                            let is_member_of_target = member_lists_set.contains(&item.list_id);

                            // An existing row carries a list of its own, and the upsert below
                            // rewrites `"listId"` from the payload. Without this second half a
                            // member of list B could take a membership row belonging to list A
                            // and move it — either dragging A's row into B, or, with someone
                            // else's row, pushing them out of B into a list they never joined.
                            let is_member_of_current = existing_map
                                .get(&change.id)
                                .map(|row| member_lists_set.contains(&row.list_id))
                                .unwrap_or(true);

                            if !is_member_of_target || !is_member_of_current {
                                return Err(AppError::Forbidden(format!(
                                    "User is not authorized to manage membership for list {}",
                                    item.list_id
                                )));
                            }

                            let record = existing_map.get(&change.id);

                            // One policy for every synced row: the server's stored version is the only
                            // input to the next one, and a row the server has never seen takes a bounded
                            // seed. `max(row.version, item.version) + 1` let a single request carrying an
                            // enormous `version` move this row's counter there permanently -- for a shared
                            // list, for every member of it. See `crate::routes::sync::versioning`.
                            let next_version = if let Some(row) = record {
                                if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                    tracing::warn!(
                                        "Conflicting write for member {} (client version {}, server version {}); accepting it as the later arrival",
                                        change.id, change.version, row.version
                                    );
                                }
                                advance_version("Member", &change.id, row.version)?
                            } else {
                                seed_version("Member", &change.id, item.version)?
                            };

                            sqlx::query!(
                                r#"
                                INSERT INTO grocery_list_members (
                                    id, "listId", "userId", role, "joinedAt", version, is_deleted, sync_state, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                                ON CONFLICT (id) DO UPDATE SET
                                    "listId" = EXCLUDED."listId",
                                    "userId" = EXCLUDED."userId",
                                    role = EXCLUDED.role,
                                    version = EXCLUDED.version,
                                    is_deleted = EXCLUDED.is_deleted,
                                    sync_state = EXCLUDED.sync_state,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                                item.id,
                                item.list_id,
                                item.user_id,
                                item.role,
                                item.joined_at,
                                next_version,
                                item.is_deleted,
                                "SYNCED",
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
                                "Failed to deserialize GroceryListMemberData for member {}: {:?}. Data: {:?}",
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
                        sqlx::query!(
                            "UPDATE grocery_list_members SET version = $1, updated_at = $2, updated_by_client = $3, sync_state = 'SYNCED' WHERE id = $4",
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

                let row = sqlx::query!(
                    "UPDATE grocery_list_members SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 WHERE id = $3 RETURNING version",
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
