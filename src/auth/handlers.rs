use axum::{extract::{State, Json}, http::{header, StatusCode}, response::{IntoResponse, Response}};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::auth::tokens::{create_access_token, hash_refresh_token, verify_refresh_token};
use rand::RngExt;
use rand::distr::Alphanumeric;

#[derive(Deserialize)]
pub struct LoginRequest {
    pub user_id: String,
    pub client_uuid: String,
    #[serde(alias = "id_token")]
    pub google_auth_token: String,
    pub use_cookie: Option<bool>,
}

#[derive(Serialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
}

#[derive(Serialize)]
pub struct BrowserAuthResponse {
    pub user_id: String,
    pub email: Option<String>,
    pub refresh_token: String,
}

#[derive(Serialize)]
pub struct BrowserRefreshResponse {
    pub refresh_token: String,
}

#[derive(Deserialize)]
pub struct RefreshRequest {
    pub user_id: String,
    pub client_uuid: String,
    pub refresh_token: String,
    pub use_cookie: Option<bool>,
}

pub async fn login_handler(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Response, StatusCode> {
    // 1. Verify Google Token (reusing existing google_client)
    let google_payload = state.google_client.validate_id_token(&payload.google_auth_token).await
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    // Manually verify that the audience claim matches either the Android or Web client ID
    if google_payload.aud != state.client_id && google_payload.aud != state.web_client_id {
        tracing::warn!("Audience mismatch: expected {} or {}, got {}", state.client_id, state.web_client_id, google_payload.aud);
        return Err(StatusCode::UNAUTHORIZED);
    }

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

    if payload.use_cookie.unwrap_or(false) {
        let cookie_header_value = if state.cookie_domain.is_empty() {
            format!(
                "access_token={}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=86400",
                access_token
            )
        } else {
            format!(
                "access_token={}; HttpOnly; Secure; SameSite=Lax; Domain={}; Path=/; Max-Age=86400",
                access_token, state.cookie_domain
            )
        };
        
        let email = google_payload.email.clone();

        let browser_response = BrowserAuthResponse {
            user_id: payload.user_id,
            email,
            refresh_token,
        };

        let mut response = Json(browser_response).into_response();
        response.headers_mut().insert(
            header::SET_COOKIE,
            header::HeaderValue::from_str(&cookie_header_value)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        );
        Ok(response)
    } else {
        Ok(Json(AuthResponse { access_token, refresh_token }).into_response())
    }
}

pub async fn refresh_handler(
    State(state): State<AppState>,
    Json(payload): Json<RefreshRequest>,
) -> Result<Response, StatusCode> {
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

    if payload.use_cookie.unwrap_or(false) {
        let cookie_header_value = if state.cookie_domain.is_empty() {
            format!(
                "access_token={}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=86400",
                access_token
            )
        } else {
            format!(
                "access_token={}; HttpOnly; Secure; SameSite=Lax; Domain={}; Path=/; Max-Age=86400",
                access_token, state.cookie_domain
            )
        };
        
        let browser_response = BrowserRefreshResponse {
            refresh_token: new_refresh_token,
        };

        let mut response = Json(browser_response).into_response();
        response.headers_mut().insert(
            header::SET_COOKIE,
            header::HeaderValue::from_str(&cookie_header_value)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
        );
        Ok(response)
    } else {
        Ok(Json(AuthResponse { access_token, refresh_token: new_refresh_token }).into_response())
    }
}

pub async fn logout_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<Response, StatusCode> {
    // 1. Try to extract access token to delete db session if possible
    let auth_header = headers.get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));

    let token = if let Some(token_val) = auth_header {
        Some(token_val.to_string())
    } else {
        headers.get(header::COOKIE)
            .and_then(|h| h.to_str().ok())
            .and_then(|cookie_str| {
                cookie_str.split(';')
                    .map(|s| s.trim())
                    .find(|s| s.starts_with("access_token="))
                    .and_then(|s| s.strip_prefix("access_token="))
            })
            .map(|t| t.to_string())
    };

    if let Some(t) = token {
        if let Ok(token_data) = jsonwebtoken::decode::<crate::auth::tokens::Claims>(
            &t,
            &jsonwebtoken::DecodingKey::from_secret(state.jwt_secret.as_bytes()),
            &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
        ) {
            // Delete the session from database
            let _ = sqlx::query!(
                "DELETE FROM sessions WHERE user_id = $1 AND client_uuid = $2",
                token_data.claims.sub,
                token_data.claims.client_uuid
            ).execute(&state.db_pool).await;
        }
    }

    // 2. Clear cookie
    let cookie_header_value = if state.cookie_domain.is_empty() {
        "access_token=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0".to_string()
    } else {
        format!(
            "access_token=; HttpOnly; Secure; SameSite=Lax; Domain={}; Path=/; Max-Age=0",
            state.cookie_domain
        )
    };
    
    let mut response = StatusCode::OK.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        header::HeaderValue::from_str(&cookie_header_value)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    Ok(response)
}
