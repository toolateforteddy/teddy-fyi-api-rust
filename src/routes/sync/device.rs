use crate::routes::sync::types::AppError;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// Placeholder used when a device registers itself without a name. Real names are the
/// three whimsical words the client generates; the server never generates one.
pub const DEFAULT_DEVICE_NAME: &str = "Tablet";

/// Resolves the device a sync request reads and writes as.
///
/// A request carrying a `device_uuid` registers that device on first sight and is rejected
/// if the id belongs to another account. A request without one falls back to the account's
/// backfilled device, which is what keeps un-upgraded tablets working during the rollout.
pub async fn resolve_sync_device(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    requested: Option<Uuid>,
    requested_name: Option<&str>,
) -> Result<Uuid, AppError> {
    match requested {
        Some(device_uuid) => {
            ensure_device(tx, user_id, device_uuid, requested_name).await?;
            Ok(device_uuid)
        }
        None => {
            let device_uuid = fallback_device(tx, user_id, requested_name).await?;
            tracing::debug!(
                "Sync request without device_uuid for user {}; falling back to device {}",
                user_id,
                device_uuid
            );
            Ok(device_uuid)
        }
    }
}

/// Resolves the device a single uploaded row belongs to, falling back to the request's
/// device when the row does not name one.
pub async fn resolve_item_device(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    item_device: Option<Uuid>,
    request_device: Uuid,
    entity: &str,
    item_id: &str,
) -> Result<Uuid, AppError> {
    match item_device {
        Some(device_uuid) if device_uuid == request_device => Ok(device_uuid),
        Some(device_uuid) => {
            ensure_device(tx, user_id, device_uuid, None).await?;
            Ok(device_uuid)
        }
        None => {
            tracing::debug!(
                "{} {} uploaded without device_uuid; falling back to device {}",
                entity,
                item_id,
                request_device
            );
            Ok(request_device)
        }
    }
}

/// Registers `device_uuid` under `user_id` when it is new, and rejects it when it is
/// already registered to a different account.
pub async fn ensure_device(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    device_uuid: Uuid,
    name: Option<&str>,
) -> Result<(), AppError> {
    let name = name.map(str::trim).filter(|n| !n.is_empty());

    let existing = sqlx::query!(
        "SELECT user_id, name FROM devices WHERE id = $1",
        device_uuid
    )
    .fetch_optional(&mut **tx)
    .await?;

    match existing {
        Some(row) if row.user_id == *user_id => {
            if let Some(name) = name {
                if name != row.name {
                    sqlx::query!(
                        "UPDATE devices SET name = $1 WHERE id = $2",
                        name,
                        device_uuid
                    )
                    .execute(&mut **tx)
                    .await?;
                }
            }
            Ok(())
        }
        Some(_) => Err(AppError::Forbidden(format!(
            "Device {} does not belong to this account",
            device_uuid
        ))),
        None => {
            tracing::info!("Registering device {} for user {}", device_uuid, user_id);
            sqlx::query!(
                "INSERT INTO devices (id, user_id, name) VALUES ($1, $2, $3)",
                device_uuid,
                user_id,
                name.unwrap_or(DEFAULT_DEVICE_NAME)
            )
            .execute(&mut **tx)
            .await?;
            Ok(())
        }
    }
}

/// The account's oldest device — the one the migration backfilled existing rows onto.
/// Creates one if the account has none yet.
pub async fn fallback_device(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    name: Option<&str>,
) -> Result<Uuid, AppError> {
    let existing = sqlx::query!(
        "SELECT id FROM devices WHERE user_id = $1 ORDER BY created_at ASC, id ASC LIMIT 1",
        user_id
    )
    .fetch_optional(&mut **tx)
    .await?;

    if let Some(row) = existing {
        return Ok(row.id);
    }

    let device_uuid = Uuid::new_v4();
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    sqlx::query!(
        "INSERT INTO devices (id, user_id, name) VALUES ($1, $2, $3)",
        device_uuid,
        user_id,
        name.unwrap_or(DEFAULT_DEVICE_NAME)
    )
    .execute(&mut **tx)
    .await?;

    Ok(device_uuid)
}

/// Records that the device just synced.
pub async fn touch_device(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    device_uuid: Uuid,
) -> Result<(), AppError> {
    sqlx::query!(
        "UPDATE devices SET last_seen_at = now() WHERE id = $1 AND user_id = $2",
        device_uuid,
        user_id
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}
