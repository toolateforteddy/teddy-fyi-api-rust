use axum::{extract::{State, Json}, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::auth::tokens::{create_access_token, hash_refresh_token, verify_refresh_token};
use rand::RngExt;
use rand::distr::Alphanumeric;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub user_id: String,
    pub client_uuid: String,
    pub google_auth_token: String,
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Deserialize)]
pub struct RefreshRequest {
    pub user_id: String,
    pub client_uuid: String,
    pub refresh_token: String,
}

pub async fn login_handler(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    // 1. Verify Google Token (reusing existing google_client)
    state.google_client.validate_id_token(&payload.google_auth_token).await
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    // 2. Generate tokens
    let access_token = create_access_token(&payload.user_id, &payload.client_uuid, state.jwt_secret.as_bytes())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let refresh_token: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();

    // 3. Upsert session
    let refresh_token_hash = hash_refresh_token(&refresh_token);
    let expiration = chrono::Utc::now() + chrono::Duration::days(7);

    sqlx::query!(
        "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (user_id, client_uuid) DO UPDATE
         SET refresh_token_hash = EXCLUDED.refresh_token_hash, expires_at = EXCLUDED.expires_at",
        payload.user_id,
        payload.client_uuid,
        refresh_token_hash,
        expiration
    ).execute(&state.db_pool).await.map_err(|e| {
        tracing::error!("Failed to upsert session: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(AuthResponse { access_token, refresh_token }))
}

pub async fn refresh_handler(
    State(state): State<AppState>,
    Json(payload): Json<RefreshRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    // 1. Get session
    let session = sqlx::query_as!(
        crate::auth::models::Session,
        "SELECT * FROM sessions WHERE user_id = $1 AND client_uuid = $2",
        payload.user_id,
        payload.client_uuid
    ).fetch_optional(&state.db_pool).await.map_err(|e| {
        tracing::error!("Database error during refresh: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?.ok_or(StatusCode::UNAUTHORIZED)?;

    // 2. Verify token
    if !verify_refresh_token(&session.refresh_token_hash, &payload.refresh_token) || session.expires_at < chrono::Utc::now() {
        // Breach mitigation: Delete all sessions
        tracing::warn!("Breach mitigation: invalidating all sessions for user {}", payload.user_id);
        sqlx::query!("DELETE FROM sessions WHERE user_id = $1", payload.user_id).execute(&state.db_pool).await.ok();
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 3. Rotate tokens
    let access_token = create_access_token(&payload.user_id, &payload.client_uuid, state.jwt_secret.as_bytes())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let new_refresh_token: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();

    let new_hash = hash_refresh_token(&new_refresh_token);
    sqlx::query!(
        "UPDATE sessions SET refresh_token_hash = $1, expires_at = $2 WHERE user_id = $3 AND client_uuid = $4",
        new_hash,
        chrono::Utc::now() + chrono::Duration::days(7),
        payload.user_id,
        payload.client_uuid
    ).execute(&state.db_pool).await.map_err(|e| {
        tracing::error!("Failed to rotate token: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(Json(AuthResponse { access_token, refresh_token: new_refresh_token }))
}
