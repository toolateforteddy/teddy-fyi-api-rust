use crate::routes::sync::types::{hash_sync_user, AppError};
use sqlx::{Postgres, Transaction};
use std::collections::HashSet;
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
                user_hash = %hash_sync_user(user_id),
                device_uuid = %device_uuid,
                "Sync request without device_uuid; falling back to the account's device"
            );
            Ok(device_uuid)
        }
    }
}

/// How a request decides which device an uploaded row belongs to.
pub enum ItemDeviceRule {
    /// The request speaks for one tablet. A row that names no device belongs to it, and a
    /// row naming another device registers that device on first sight.
    RequestDevice(Uuid),
    /// The caller manages devices it is not itself running on — the cloud app picks a
    /// device from a dropdown, so the row names its subject, not the request. Every row
    /// must name a device, and that device must already be registered to the account:
    /// the cloud app can only manage tablets that exist, never bring one into being.
    RowMustName,
}

/// Resolves the device a single uploaded row belongs to.
pub async fn resolve_item_device(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    item_device: Option<Uuid>,
    rule: &ItemDeviceRule,
    entity: &str,
    item_id: &str,
) -> Result<Uuid, AppError> {
    match (rule, item_device) {
        (ItemDeviceRule::RequestDevice(request_device), Some(device_uuid))
            if device_uuid == *request_device =>
        {
            Ok(device_uuid)
        }
        (ItemDeviceRule::RequestDevice(_), Some(device_uuid)) => {
            ensure_device(tx, user_id, device_uuid, None).await?;
            Ok(device_uuid)
        }
        (ItemDeviceRule::RequestDevice(request_device), None) => {
            tracing::debug!(
                "{} {} uploaded without device_uuid; falling back to device {}",
                entity,
                item_id,
                request_device
            );
            Ok(*request_device)
        }
        (ItemDeviceRule::RowMustName, Some(device_uuid)) => {
            require_registered_device(tx, user_id, device_uuid).await?;
            Ok(device_uuid)
        }
        (ItemDeviceRule::RowMustName, None) => Err(AppError::BadRequest(format!(
            "{} {} must carry a device_uuid",
            entity, item_id
        ))),
    }
}

/// The devices out of `candidates` that are already registered to this account.
///
/// [`ItemDeviceRule::RowMustName`] checks one row's device at a time, and a payload of ten
/// thousand rows names the same handful of tablets over and over — the same statement, with
/// the same parameters, ten thousand times. Resolving the distinct ids once turns the check
/// in the loop into a set membership test.
///
/// The set cannot go stale under the loop: nothing on the `RowMustName` path creates a
/// device, and a `RequestDevice` batch does not consult the set at all. A miss still falls
/// through to [`require_registered_device`] anyway (see [`resolve_item_device_cached`]), so
/// a device some other part of the same transaction registered is found rather than wrongly
/// refused.
///
/// Empty for [`ItemDeviceRule::RequestDevice`], which registers unknown ids instead of
/// refusing them and so has nothing to pre-resolve.
pub async fn registered_device_set(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    rule: &ItemDeviceRule,
    candidates: &[Uuid],
) -> Result<HashSet<Uuid>, AppError> {
    if !matches!(rule, ItemDeviceRule::RowMustName) || candidates.is_empty() {
        return Ok(HashSet::new());
    }

    let ids = sqlx::query_scalar!(
        "SELECT id FROM devices WHERE id = ANY($1) AND user_id = $2",
        candidates,
        user_id
    )
    .fetch_all(&mut **tx)
    .await?;

    Ok(ids.into_iter().collect())
}

/// [`resolve_item_device`] with the `RowMustName` lookup already done for the whole batch.
///
/// A hit answers from `registered`; anything else — a different rule, a device the prefetch
/// did not see, a device that is not registered — is handed to `resolve_item_device`
/// unchanged. That is what keeps the security property intact: an id this account does not
/// own still travels the original path and comes back with the same `NotFound`, whether it
/// belongs to nobody or to somebody else.
#[allow(clippy::too_many_arguments)]
pub async fn resolve_item_device_cached(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    item_device: Option<Uuid>,
    rule: &ItemDeviceRule,
    entity: &str,
    item_id: &str,
    registered: &HashSet<Uuid>,
) -> Result<Uuid, AppError> {
    if let (ItemDeviceRule::RowMustName, Some(device_uuid)) = (rule, item_device) {
        if registered.contains(&device_uuid) {
            return Ok(device_uuid);
        }
    }

    resolve_item_device(tx, user_id, item_device, rule, entity, item_id).await
}

/// Asserts the device is already registered to this account, without creating it.
///
/// Unknown ids and ids owned by another account are reported identically, so this cannot
/// be used to probe which device ids exist.
pub async fn require_registered_device(
    tx: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    device_uuid: Uuid,
) -> Result<(), AppError> {
    let owned = sqlx::query!(
        "SELECT 1 as one FROM devices WHERE id = $1 AND user_id = $2",
        device_uuid,
        user_id
    )
    .fetch_optional(&mut **tx)
    .await?;

    if owned.is_none() {
        return Err(AppError::NotFound(format!(
            "Device {} is not registered to this account",
            device_uuid
        )));
    }
    Ok(())
}

/// Registers `device_uuid` under `user_id` when it is new, and rejects it when it is
/// already registered to a different account.
///
/// This is the only place in the service that creates a `devices` row from a
/// caller-supplied id, so it is also where the per-account cap
/// ([`crate::routes::devices::limits::max_devices_per_account`]) is enforced. Putting the
/// cap on `POST /api/devices` alone would have been decorative: a sync request naming an
/// unknown `device_uuid` lands here too and would have gone on minting rows for free.
///
/// The cap is checked *only* on the branch that would insert. A device already registered
/// to this account returns above it, so a real tablet re-registering — which is what every
/// launch does — can never be refused, however far over the cap the account happens to be.
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
            // Counted here rather than once per request because this is the branch that
            // grows the table, and because a single sync request can name several devices.
            let registered = sqlx::query_scalar!(
                r#"SELECT COUNT(*) AS "count!" FROM devices WHERE user_id = $1"#,
                user_id
            )
            .fetch_one(&mut **tx)
            .await?;

            if registered >= crate::routes::devices::limits::max_devices_per_account() {
                // 429 and not 403: nothing about the request is wrong, the account simply
                // has no room. Removing a device it no longer uses makes this succeed.
                tracing::warn!(
                    user_hash = %hash_sync_user(user_id),
                    registered,
                    "Device registration refused: per-account device cap reached"
                );
                return Err(AppError::TooManyRequests(format!(
                    "Account already has the maximum of {} registered devices",
                    crate::routes::devices::limits::max_devices_per_account()
                )));
            }

            tracing::info!(
                user_hash = %hash_sync_user(user_id),
                device_uuid = %device_uuid,
                "Registering device"
            );
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
///
/// Deliberately not capped: it inserts only when the account has *zero* devices, so it can
/// never be the call that takes an account past its limit, and a cap here could only ever
/// misfire.
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

/// The account's fallback device, without registering one.
///
/// `fallback_device` is the write path's version: a sync request with no `device_uuid`
/// creates a device when the account has none. Read-only callers — the SSE stream — must
/// not bring a device into being just by connecting, so this reports `None` instead and
/// leaves the caller account-wide. Same ordering, so both paths pick the same device.
pub async fn existing_fallback_device(
    pool: &sqlx::PgPool,
    user_id: &Uuid,
) -> Result<Option<Uuid>, AppError> {
    let row = sqlx::query!(
        "SELECT id FROM devices WHERE user_id = $1 ORDER BY created_at ASC, id ASC LIMIT 1",
        user_id
    )
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|row| row.id))
}
