use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn process_grocery_list_member_changes(
    tx: &mut Transaction<'_, Postgres>,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[GroceryListMemberChangeDelta],
    success_ids: &mut Vec<String>,
) -> Result<(), AppError> {
    for change in changes {
        match change.operation_type {
            OperationType::Insert => {
                tracing::info!("Inserting grocery list member {}", change.id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<GroceryListMemberData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!(
                                "SELECT version FROM grocery_list_members WHERE id = $1",
                                change.id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            let next_version = if let Some(row) = record {
                                std::cmp::max(row.version, item.version) + 1
                            } else {
                                item.version
                            };

                            sqlx::query(
                                r#"
                                INSERT INTO grocery_list_members (
                                    id, "listId", "userId", role, "joinedAt", version, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                                ON CONFLICT (id) DO UPDATE SET
                                    "listId" = EXCLUDED."listId",
                                    "userId" = EXCLUDED."userId",
                                    role = EXCLUDED.role,
                                    version = EXCLUDED.version,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                            )
                            .bind(&item.id)
                            .bind(&item.list_id)
                            .bind(&item.user_id)
                            .bind(&item.role)
                            .bind(item.joined_at)
                            .bind(next_version)
                            .bind(server_timestamp)
                            .bind(client_id)
                            .execute(&mut **tx)
                            .await?;
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize GroceryListMemberData for member {}: {:?}",
                                change.id,
                                err
                            );
                        }
                    }
                }
                success_ids.push(change.id.clone());
            }
            OperationType::Update => {
                tracing::info!("Updating grocery list member {}", change.id);
                if let Some(ref data) = change.data {
                    match serde_json::from_value::<GroceryListMemberData>(data.clone()) {
                        Ok(item) => {
                            let record = sqlx::query!(
                                "SELECT version FROM grocery_list_members WHERE id = $1",
                                change.id
                            )
                            .fetch_optional(&mut **tx)
                            .await?;

                            let next_version = if let Some(row) = record {
                                if change.version < row.version {
                                    tracing::warn!(
                                        "MVCC Conflict for member {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                        change.id, change.version, row.version
                                    );
                                }
                                std::cmp::max(row.version, change.version) + 1
                            } else {
                                change.version
                            };

                            sqlx::query(
                                r#"
                                INSERT INTO grocery_list_members (
                                    id, "listId", "userId", role, "joinedAt", version, updated_at, updated_by_client
                                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                                ON CONFLICT (id) DO UPDATE SET
                                    "listId" = EXCLUDED."listId",
                                    "userId" = EXCLUDED."userId",
                                    role = EXCLUDED.role,
                                    version = EXCLUDED.version,
                                    updated_at = EXCLUDED.updated_at,
                                    updated_by_client = EXCLUDED.updated_by_client
                                "#,
                            )
                            .bind(&item.id)
                            .bind(&item.list_id)
                            .bind(&item.user_id)
                            .bind(&item.role)
                            .bind(item.joined_at)
                            .bind(next_version)
                            .bind(server_timestamp)
                            .bind(client_id)
                            .execute(&mut **tx)
                            .await?;
                        }
                        Err(err) => {
                            tracing::error!(
                                "Failed to deserialize GroceryListMemberData for member {}: {:?}",
                                change.id,
                                err
                            );
                        }
                    }
                } else {
                    let record = sqlx::query!(
                        "SELECT version FROM grocery_list_members WHERE id = $1",
                        change.id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    if let Some(row) = record {
                        let next_version = row.version + 1;
                        if change.version < row.version {
                            tracing::warn!(
                                "MVCC Conflict for member {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                change.id, change.version, row.version
                            );
                        }

                        sqlx::query!(
                            "UPDATE grocery_list_members SET version = $1, updated_at = $2, updated_by_client = $3 WHERE id = $4",
                            next_version,
                            server_timestamp,
                            client_id,
                            change.id
                        )
                        .execute(&mut **tx)
                        .await?;
                    }
                }
                success_ids.push(change.id.clone());
            }
            OperationType::Delete => {
                sqlx::query!("DELETE FROM grocery_list_members WHERE id = $1", change.id)
                    .execute(&mut **tx)
                    .await?;
                success_ids.push(change.id.clone());
            }
        }
    }
    Ok(())
}
