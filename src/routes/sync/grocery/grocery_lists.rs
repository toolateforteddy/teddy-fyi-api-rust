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
) -> Result<(), AppError> {
    for change in changes {
        match change.operation_type {
            OperationType::Insert | OperationType::Update => {
                tracing::info!("Processing grocery list {}", change.id);
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
                                "Failed to deserialize GroceryListData for grocery list {}: {:?}",
                                change.id,
                                err
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
            }
        }
    }
    Ok(())
}
