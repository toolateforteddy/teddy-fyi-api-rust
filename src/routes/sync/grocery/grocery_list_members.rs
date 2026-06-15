use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn process_grocery_list_member_changes(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &str,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[GroceryListMemberChangeDelta],
    success_ids: &mut Vec<String>,
    upload_status: &mut Vec<SuccessResult>,
) -> Result<(), AppError> {
    for change in changes {
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing grocery list member {}", change.id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<GroceryListMemberData>(data.clone()) {
                        Ok(item) => {
                            // Verify permission: User must either be joining themselves, or already be a member of the list
                            let is_joining_self = item.user_id == user_id;
                            let is_already_member = sqlx::query!(
                                r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                                item.list_id,
                                user_id
                            )
                            .fetch_optional(&mut **tx)
                            .await?
                            .is_some();

                            if !is_joining_self && !is_already_member {
                                return Err(AppError::Forbidden(format!(
                                    "User is not authorized to manage membership for list {}",
                                    item.list_id
                                )));
                            }

                            let record = sqlx::query!(
                                "SELECT version FROM grocery_list_members WHERE id = $1",
                                change.id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            let next_version = if let Some(row) = record {
                                if matches!(change.operation_type, OperationType::Update) && change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for member {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.id, change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
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
                                "Failed to deserialize GroceryListMemberData for member {}: {:?}",
                                change.id,
                                err
                            );
                            return Err(AppError::Serialization(err));
                        }
                    }
                } else if matches!(change.operation_type, OperationType::Update) {
                    let member_record = sqlx::query!(
                        r#"SELECT "listId" as list_id, "userId" as user_id FROM grocery_list_members WHERE id = $1"#,
                        change.id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(member_rec) = member_record {
                        let is_self = member_rec.user_id == user_id;
                        let is_member = sqlx::query!(
                            r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                            member_rec.list_id,
                            user_id
                        )
                        .fetch_optional(&mut **tx)
                        .await?
                        .is_some();

                        if !is_self && !is_member {
                            return Err(AppError::Forbidden(format!(
                                "User is not authorized to update membership {}",
                                change.id
                            )));
                        }
                    }

                    let record = sqlx::query!(
                        "SELECT version FROM grocery_list_members WHERE id = $1",
                        change.id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = record {
                        let next_version = row.version + 1;
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
                let member_record = sqlx::query!(
                    r#"SELECT "listId" as list_id, "userId" as user_id FROM grocery_list_members WHERE id = $1"#,
                    change.id
                )
                .fetch_optional(&mut **tx)
                .await?;

                if let Some(member_rec) = member_record {
                    let is_self = member_rec.user_id == user_id;
                    let is_member = sqlx::query!(
                        r#"SELECT 1 as dummy FROM grocery_list_members WHERE "listId" = $1 AND "userId" = $2 AND is_deleted = FALSE"#,
                        member_rec.list_id,
                        user_id
                    )
                    .fetch_optional(&mut **tx)
                    .await?
                    .is_some();

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
