use axum::{
    extract::{Path, State},
    Extension, Json,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::tokens::Claims;
use crate::routes::sync::remote_mutations::parse_or_hash_uuid;
use crate::routes::sync::types::AppError;
use crate::state::AppState;

/// One tablet on the account. `id` is the `device_uuid` that scopes configs and drawings;
/// `name` is only ever a label for it.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeviceResponse {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DeviceListResponse {
    pub devices: Vec<DeviceResponse>,
}

#[derive(Debug, Deserialize)]
pub struct RegisterDeviceRequest {
    /// The tablet's own id. Omit only when the client has none yet; the server then
    /// allocates one and returns it.
    #[serde(default, alias = "deviceUuid")]
    pub device_uuid: Option<Uuid>,
    /// Generated on the client. The server stores it verbatim and never invents one.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RenameDeviceRequest {
    pub name: String,
}

/// Longest device name this endpoint accepts, in Unicode scalar values (`char`s) rather
/// than bytes, so the limit does not silently shrink for a name written in Japanese or
/// containing emoji. A device name is a human label for a tablet — "Ada's iPad" — so this
/// is already far more room than anyone needs.
pub const MAX_DEVICE_NAME_CHARS: usize = 100;

/// Cheap byte pre-filter before counting characters: UTF-8 uses at most 4 bytes per
/// scalar value, so anything longer than this cannot be within the character limit.
const MAX_DEVICE_NAME_BYTES: usize = MAX_DEVICE_NAME_CHARS * 4;

/// Validates a device name supplied by a client.
///
/// Both failures are `BadRequest`: the caller is entitled to rename their own device, the
/// payload is simply not one the server will store. `Forbidden` would say the opposite —
/// that the tablet is not theirs — which sends a client looking for an authorization
/// problem that is not there.
pub(crate) fn validate_device_name(name: &str) -> Result<(), AppError> {
    if name.trim().is_empty() {
        return Err(AppError::BadRequest(
            "Device name must not be empty".to_string(),
        ));
    }
    if name.len() > MAX_DEVICE_NAME_BYTES || name.chars().count() > MAX_DEVICE_NAME_CHARS {
        return Err(AppError::BadRequest(format!(
            "Device name must be at most {} characters",
            MAX_DEVICE_NAME_CHARS
        )));
    }
    Ok(())
}

/// `GET /api/devices` — every tablet on the authenticated account.
pub async fn list_devices_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<DeviceListResponse>, AppError> {
    let user_uuid = parse_or_hash_uuid(&claims.sub);

    let rows = sqlx::query!(
        "SELECT id, name, created_at, last_seen_at FROM devices WHERE user_id = $1 ORDER BY created_at ASC, id ASC",
        user_uuid
    )
    .fetch_all(&state.db_pool)
    .await?;

    let devices = rows
        .into_iter()
        .map(|row| DeviceResponse {
            id: row.id,
            name: row.name,
            created_at: row.created_at,
            last_seen_at: row.last_seen_at,
        })
        .collect();

    Ok(Json(DeviceListResponse { devices }))
}

/// `POST /api/devices` — register a tablet, or update the name of one already registered.
pub async fn register_device_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(payload): Json<RegisterDeviceRequest>,
) -> Result<Json<DeviceResponse>, AppError> {
    let user_uuid = parse_or_hash_uuid(&claims.sub);
    let device_uuid = payload.device_uuid.unwrap_or_else(Uuid::new_v4);
    // An absent or blank name means "no name supplied" on registration — the device layer
    // then keeps whatever name it already has, or generates one — so only a name the
    // caller actually meant is validated.
    let name = payload
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    if let Some(name) = name {
        validate_device_name(name)?;
    }

    let mut tx = state.db_pool.begin().await?;
    crate::routes::sync::device::ensure_device(&mut tx, &user_uuid, device_uuid, name).await?;
    tx.commit().await?;

    fetch_device(&state, &user_uuid, device_uuid).await.map(Json)
}

/// `PATCH /api/devices/:id` — rename a tablet the caller owns.
pub async fn rename_device_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(device_uuid): Path<Uuid>,
    Json(payload): Json<RenameDeviceRequest>,
) -> Result<Json<DeviceResponse>, AppError> {
    let user_uuid = parse_or_hash_uuid(&claims.sub);
    validate_device_name(&payload.name)?;
    let name = payload.name.trim();

    let updated = sqlx::query!(
        "UPDATE devices SET name = $1 WHERE id = $2 AND user_id = $3",
        name,
        device_uuid,
        user_uuid
    )
    .execute(&state.db_pool)
    .await?;

    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Device {} not found", device_uuid)));
    }

    fetch_device(&state, &user_uuid, device_uuid).await.map(Json)
}

async fn fetch_device(
    state: &AppState,
    user_uuid: &Uuid,
    device_uuid: Uuid,
) -> Result<DeviceResponse, AppError> {
    let row = sqlx::query!(
        "SELECT id, name, created_at, last_seen_at FROM devices WHERE id = $1 AND user_id = $2",
        device_uuid,
        user_uuid
    )
    .fetch_optional(&state.db_pool)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Device {} not found", device_uuid)))?;

    Ok(DeviceResponse {
        id: row.id,
        name: row.name,
        created_at: row.created_at,
        last_seen_at: row.last_seen_at,
    })
}

#[cfg(test)]
mod tests;
