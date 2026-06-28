use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

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
    for change in changes {
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing grocery list {}", change.id);

                let is_need_update = matches!(change.operation_type, OperationType::Update)
                    && (change.data.is_none() || change.data.as_ref().map(|v| v.is_null()).unwrap_or(false));

                if is_need_update {
                    let existing = sqlx::query!(
                        r#"SELECT name, "ownerId" as owner_id, "createdAt" as created_at, version, is_deleted, sync_state FROM grocery_lists WHERE id = $1"#,
                        change.id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = existing {
                        let is_owner = row.owner_id.as_deref() == Some(user_id);
                        let mut authorized = is_owner;
                        if !authorized {
                            let is_member = sqlx::query!(
                                r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                                change.id,
                                user_id
                            )
                            .fetch_optional(&mut **tx)
                            .await?
                            .is_some();
                            if is_member {
                                authorized = true;
                            }
                        }
                        if !authorized {
                            return Err(AppError::Forbidden(format!("User is not authorized to update grocery list {}", change.id)));
                        }

                        let item_data = GroceryListData {
                            id: change.id.clone(),
                            name: row.name,
                            owner_id: row.owner_id,
                            created_at: row.created_at,
                            version: row.version,
                            is_deleted: row.is_deleted,
                            sync_state: row.sync_state,
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
                            let record = sqlx::query!(
                                "SELECT version FROM grocery_lists WHERE id = $1",
                                change.id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            if record.is_some() && matches!(change.operation_type, OperationType::Update) {
                                // For Update, verify user is a member of the list
                                let is_member = sqlx::query!(
                                    r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                                    change.id,
                                    user_id
                                )
                                .fetch_optional(&mut **tx)
                                .await?;
                                if is_member.is_none() {
                                    return Err(AppError::Forbidden(format!("User is not a member of grocery list {}", change.id)));
                                }
                            }

                            let next_version = if let Some(row) = record {
                                if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for grocery list {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.id, change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
                            };

                            sqlx::query!(
                                r#"
                                INSERT INTO grocery_lists (
                                    id, name, "ownerId", "createdAt", version, is_deleted, sync_state, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                                ON CONFLICT (id) DO UPDATE SET
                                    name = EXCLUDED.name,
                                    "ownerId" = EXCLUDED."ownerId",
                                    version = EXCLUDED.version,
                                    is_deleted = EXCLUDED.is_deleted,
                                    sync_state = EXCLUDED.sync_state,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                                item.id,
                                item.name,
                                item.owner_id,
                                item.created_at,
                                next_version,
                                item.is_deleted,
                                "SYNCED",
                                server_timestamp,
                                client_id
                            )
                            .execute(&mut **tx)
                            .await?;

                            // Automatically add the creator as an ADMIN member of the list if not already
                            let member_exists = sqlx::query!(
                                r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2"#,
                                item.id,
                                user_id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            if member_exists.is_none() {
                                let member_id = format!("{}-member-{}", item.id, user_id);
                                sqlx::query!(
                                    r#"INSERT INTO grocery_list_members (id, "listId", "userId", role, "joinedAt", version, sync_state, updated_at, updated_by_client)
                                       VALUES ($1, $2, $3, $4, $5, 1, 'SYNCED', $6, NULL)
                                       ON CONFLICT (id) DO NOTHING"#,
                                    member_id,
                                    item.id,
                                    user_id,
                                    "ADMIN",
                                    item.created_at,
                                    server_timestamp
                                )
                                .execute(&mut **tx)
                                .await?;
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
                                "Failed to deserialize GroceryListData for grocery list {}: {:?}. Data: {:?}",
                                change.id,
                                err,
                                data
                            );
                            return Err(AppError::Serialization(err));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let is_member = sqlx::query!(
                        r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                        change.id,
                        user_id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;
                    if is_member.is_none() {
                        return Err(AppError::Forbidden(format!("User is not a member of grocery list {}", change.id)));
                    }

                    let record =
                        sqlx::query!("SELECT version FROM grocery_lists WHERE id = $1", change.id)
                            .fetch_optional(&mut **tx)
                            .await?;

                    if let Some(row) = record {
                        let next_version = row.version + 1;
                        sqlx::query!(
                            "UPDATE grocery_lists SET version = $1, updated_at = $2, updated_by_client = $3, sync_state = 'SYNCED' WHERE id = $4",
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
                let existing_list = sqlx::query!(
                    r#"SELECT "ownerId" as owner_id, version, is_deleted FROM grocery_lists WHERE id = $1"#,
                    change.id
                )
                .fetch_optional(&mut **tx)
                .await?;

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
                    None => {
                        return Err(AppError::Forbidden(format!("Grocery list {} not found", change.id)));
                    }
                };

                let member_rec = sqlx::query!(
                    r#"SELECT id, role, is_deleted FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2"#,
                    change.id,
                    user_id
                )
                .fetch_optional(&mut **tx)
                .await?;

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

                let is_owner = existing_list.as_ref().and_then(|l| l.owner_id.as_deref()) == Some(user_id)
                    || member_row.role == "OWNER";

                if is_owner {
                    let row = sqlx::query!(
                        "UPDATE grocery_lists SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 WHERE id = $3 RETURNING version",
                        server_timestamp,
                        client_id,
                        change.id
                    )
                    .fetch_one(&mut **tx)
                    .await?;

                    // Soft delete associated grocery items
                    sqlx::query!(
                        r#"UPDATE grocery_items 
                           SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 
                           WHERE "listId" = $3 AND is_deleted = FALSE"#,
                        server_timestamp,
                        client_id,
                        change.id
                    )
                    .execute(&mut **tx)
                    .await?;

                    // Soft delete associated grocery list members
                    sqlx::query!(
                        r#"UPDATE grocery_list_members 
                           SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 
                           WHERE "listId" = $3 AND is_deleted = FALSE"#,
                        server_timestamp,
                        client_id,
                        change.id
                    )
                    .execute(&mut **tx)
                    .await?;

                    // Soft delete associated stores tied to this list
                    sqlx::query!(
                        r#"UPDATE stores
                           SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2
                           WHERE "listId" = $3 AND is_deleted = FALSE"#,
                        server_timestamp,
                        client_id,
                        change.id
                    )
                    .execute(&mut **tx)
                    .await?;

                    // Soft delete associated categories tied to this list
                    sqlx::query!(
                        r#"UPDATE categories
                           SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2
                           WHERE "listId" = $3 AND is_deleted = FALSE"#,
                        server_timestamp,
                        client_id,
                        change.id
                    )
                    .execute(&mut **tx)
                    .await?;

                    // Hard delete associated list invites
                    sqlx::query!(
                        r#"DELETE FROM list_invites WHERE "listId" = $1"#,
                        change.id
                    )
                    .execute(&mut **tx)
                    .await?;

                    upload_status.push(SuccessResult {
                        id: change.id.clone(),
                        version: row.version,
                        sync_state: "SYNCED".to_string(),
                    });
                    success_ids.push(change.id.clone());
                } else {
                    // Non-owner member deleting list: only soft-delete their own membership
                    sqlx::query!(
                        r#"UPDATE grocery_list_members 
                           SET is_deleted = TRUE, version = version + 1, updated_at = $1, updated_by_client = $2 
                           WHERE id = $3"#,
                        server_timestamp,
                        client_id,
                        member_row.id
                    )
                    .execute(&mut **tx)
                    .await?;

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
    Ok(())
}
