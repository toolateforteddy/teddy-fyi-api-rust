use crate::models::{Drawing, SyncState};
use uuid::Uuid;

pub struct DrawingDao;

impl DrawingDao {
    /// Fetches a drawing by its ID, scoped to a specific user to ensure data isolation.
    pub async fn get_by_id(
        pool: &sqlx::PgPool,
        id: Uuid,
        user_id: Uuid,
    ) -> Result<Option<Drawing>, sqlx::Error> {
        sqlx::query_as::<_, Drawing>(
            "SELECT id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data \
             FROM drawings WHERE id = $1 AND user_id = $2"
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(pool)
        .await
    }

    /// Lists all active drawings for a user.
    pub async fn list_for_user(
        pool: &sqlx::PgPool,
        user_id: Uuid,
    ) -> Result<Vec<Drawing>, sqlx::Error> {
        sqlx::query_as::<_, Drawing>(
            "SELECT id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data \
             FROM drawings WHERE user_id = $1 AND is_deleted = FALSE"
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
    }

    /// Fetches drawings that have pending synchronization changes for a specific client.
    pub async fn get_pending_sync(
        pool: &sqlx::PgPool,
        user_id: Uuid,
        client_uuid: Uuid,
    ) -> Result<Vec<Drawing>, sqlx::Error> {
        sqlx::query_as::<_, Drawing>(
            "SELECT id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data \
             FROM drawings \
             WHERE user_id = $1 AND client_uuid = $2 AND sync_state != 'SYNCED'"
        )
        .bind(user_id)
        .bind(client_uuid)
        .fetch_all(pool)
        .await
    }

    /// Performs an upsert with MVCC version conflict detection and Last-Write-Wins (LWW) resolution.
    pub async fn upsert(
        pool: &sqlx::PgPool,
        user_id: Uuid,
        incoming: &Drawing,
    ) -> Result<Drawing, sqlx::Error> {
        // Fetch current server state of the drawing, ensuring user isolation
        let existing = Self::get_by_id(pool, incoming.id, user_id).await?;

        match existing {
            None => {
                // Drawing doesn't exist yet, insert directly
                sqlx::query_as::<_, Drawing>(
                    "INSERT INTO drawings (id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data) \
                     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
                     RETURNING id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data"
                )
                .bind(incoming.id)
                .bind(user_id)
                .bind(incoming.client_uuid)
                .bind(incoming.version)
                .bind(incoming.is_deleted)
                .bind(incoming.last_modified)
                .bind(incoming.sync_state)
                .bind(incoming.created_at)
                .bind(&incoming.data)
                .fetch_one(pool)
                .await
            }
            Some(existing_record) => {
                // Conflict resolution: compare versions and last_modified times
                let next_version = if incoming.version == existing_record.version {
                    // Match: normal incremental update
                    existing_record.version + 1
                } else if incoming.version < existing_record.version {
                    // Conflict: Client is behind. Resolve with LWW using last_modified
                    if incoming.last_modified >= existing_record.last_modified {
                        // Client write has newer/equal timestamp. Overwrite and bump version
                        existing_record.version + 1
                    } else {
                        // Server write is newer. Keep server state, reject update.
                        return Ok(existing_record);
                    }
                } else {
                    // Client is ahead (version > server_version)
                    incoming.version + 1
                };

                sqlx::query_as::<_, Drawing>(
                    "UPDATE drawings \
                     SET client_uuid = $1, version = $2, is_deleted = $3, last_modified = $4, sync_state = $5, data = $6 \
                     WHERE id = $7 AND user_id = $8 \
                     RETURNING id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data"
                )
                .bind(incoming.client_uuid)
                .bind(next_version)
                .bind(incoming.is_deleted)
                .bind(incoming.last_modified)
                .bind(incoming.sync_state)
                .bind(&incoming.data)
                .bind(incoming.id)
                .bind(user_id)
                .fetch_one(pool)
                .await
            }
        }
    }

    /// Soft deletes a drawing by marking it as deleted and updating its sync version and last_modified timestamp.
    pub async fn soft_delete(
        pool: &sqlx::PgPool,
        id: Uuid,
        user_id: Uuid,
        client_uuid: Uuid,
        epoch_millis: i64,
    ) -> Result<Option<Drawing>, sqlx::Error> {
        let existing = Self::get_by_id(pool, id, user_id).await?;

        if let Some(existing_record) = existing {
            let next_version = existing_record.version + 1;
            let updated = sqlx::query_as::<_, Drawing>(
                "UPDATE drawings \
                 SET is_deleted = TRUE, version = $1, last_modified = $2, client_uuid = $3, sync_state = $4 \
                 WHERE id = $5 AND user_id = $6 \
                 RETURNING id, user_id, client_uuid, version, is_deleted, last_modified, sync_state, created_at, data"
            )
            .bind(next_version)
            .bind(epoch_millis)
            .bind(client_uuid)
            .bind(SyncState::PendingDelete)
            .bind(id)
            .bind(user_id)
            .fetch_one(pool)
            .await?;

            Ok(Some(updated))
        } else {
            Ok(None)
        }
    }
}
