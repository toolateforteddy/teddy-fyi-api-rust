use crate::routes::sync::types::*;
use chrono::{DateTime, Utc};
use sqlx::{Postgres, Transaction};

pub async fn process_category_changes(
    tx: &mut Transaction<'_, Postgres>,
    client_id: &str,
    server_timestamp: DateTime<Utc>,
    changes: &[CategoryChangeDelta],
    success_ids: &mut Vec<String>,
) -> Result<(), AppError> {
    for change in changes {
        let string_id = change.id.to_string();
        match change.operation_type {
            OperationType::Insert => {
                tracing::info!("Inserting category {}", change.id);
                if let Some(ref data) = change.data {
                    let item = serde_json::from_value::<CategoryData>(data.clone())?;
                    let record = sqlx::query!(
                        "SELECT version FROM categories WHERE id = $1",
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
                        INSERT INTO categories (
                            id, name, position, "userId", version, updated_at, updated_by_client
                        ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                        ON CONFLICT (id) DO UPDATE SET
                            name = EXCLUDED.name,
                            position = EXCLUDED.position,
                            "userId" = EXCLUDED."userId",
                            version = EXCLUDED.version,
                            updated_at = EXCLUDED.updated_at,
                            updated_by_client = EXCLUDED.updated_by_client
                        "#,
                    )
                    .bind(item.id)
                    .bind(&item.name)
                    .bind(item.position)
                    .bind(&item.user_id)
                    .bind(next_version)
                    .bind(server_timestamp)
                    .bind(client_id)
                    .execute(&mut **tx)
                    .await?;
                }
                success_ids.push(string_id);
            }
            OperationType::Update => {
                tracing::info!("Updating category {}", change.id);
                if let Some(ref data) = change.data {
                    let item = serde_json::from_value::<CategoryData>(data.clone())?;
                    let record = sqlx::query!(
                        "SELECT version FROM categories WHERE id = $1",
                        change.id
                    )
                    .fetch_optional(&mut **tx)
                    .await?;

                    let next_version = if let Some(row) = record {
                        if change.version < row.version {
                            tracing::warn!(
                                "MVCC Conflict for category {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                change.id, change.version, row.version
                            );
                        }
                        std::cmp::max(row.version, change.version) + 1
                    } else {
                        change.version
                    };

                    sqlx::query(
                        r#"
                        INSERT INTO categories (
                            id, name, position, "userId", version, updated_at, updated_by_client
                        ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                        ON CONFLICT (id) DO UPDATE SET
                            name = EXCLUDED.name,
                            position = EXCLUDED.position,
                            "userId" = EXCLUDED."userId",
                            version = EXCLUDED.version,
                            updated_at = EXCLUDED.updated_at,
                            updated_by_client = EXCLUDED.updated_by_client
                        "#,
                    )
                    .bind(item.id)
                    .bind(&item.name)
                    .bind(item.position)
                    .bind(&item.user_id)
                    .bind(next_version)
                    .bind(server_timestamp)
                    .bind(client_id)
                    .execute(&mut **tx)
                    .await?;
                } else {
                    let record =
                        sqlx::query!("SELECT version FROM categories WHERE id = $1", change.id)
                            .fetch_optional(&mut **tx)
                            .await?;

                    if let Some(row) = record {
                        let next_version = row.version + 1;
                        if change.version < row.version {
                            tracing::warn!(
                                "MVCC Conflict for category {}. Client version: {}, Server version: {}. Resolving via LWW.",
                                change.id, change.version, row.version
                            );
                        }

                        sqlx::query!(
                            "UPDATE categories SET version = $1, updated_at = $2, updated_by_client = $3 WHERE id = $4",
                            next_version,
                            server_timestamp,
                            client_id,
                            change.id
                        )
                        .execute(&mut **tx)
                        .await?;
                    }
                }
                success_ids.push(string_id);
            }
            OperationType::Delete => {
                sqlx::query!("DELETE FROM categories WHERE id = $1", change.id)
                    .execute(&mut **tx)
                    .await?;
                success_ids.push(string_id);
            }
        }
    }
    Ok(())
}
