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
    pub expires_in_secs: Option<i64>,
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
    pub expires_in_secs: Option<i64>,
}

pub async fn login_handler(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Response, StatusCode> {
    // 1. Resolve user ID and email (supporting dev bypass)
    let (user_id, email) = if payload.google_auth_token.starts_with("mock.") && state.cookie_domain.is_empty() {
        (payload.user_id.clone(), Some("dev-user@teddy.fyi".to_string()))
    } else {
        // Verify Google Token (reusing existing google_client)
        let google_payload = state.google_client.validate_id_token(&payload.google_auth_token).await
            .map_err(|_| StatusCode::UNAUTHORIZED)?;

        if !state.google_client_ids.contains(&google_payload.aud) {
            tracing::warn!(
                "Audience mismatch: expected one of {:?}, got {}",
                state.google_client_ids,
                google_payload.aud
            );
            return Err(StatusCode::UNAUTHORIZED);
        }
        (payload.user_id.clone(), google_payload.email.clone())
    };

    let duration_secs = payload.expires_in_secs.unwrap_or(86400);
    let duration_secs = if duration_secs <= 0 || duration_secs > 86400 {
        86400
    } else {
        duration_secs
    };

    // 2. Generate tokens
    let access_token = create_access_token(
        &user_id,
        &payload.client_uuid,
        state.jwt_secret.as_bytes(),
        Some(duration_secs),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let refresh_token: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();

    // 3. Upsert user info in users table
    sqlx::query!(
        r#"INSERT INTO users (id, email)
           VALUES ($1, $2)
           ON CONFLICT (id) DO UPDATE SET email = COALESCE(EXCLUDED.email, users.email), updated_at = NOW()"#,
        user_id,
        email
    )
    .execute(&state.db_pool)
    .await
    .map_err(|e| {
        tracing::error!("Failed to upsert user: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // 4. Upsert session
    let refresh_token_hash = hash_refresh_token(&refresh_token);
    let expiration = chrono::Utc::now() + chrono::Duration::days(7);

    sqlx::query!(
        "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (user_id, client_uuid) DO UPDATE
         SET refresh_token_hash = EXCLUDED.refresh_token_hash, expires_at = EXCLUDED.expires_at, old_refresh_token_hash = EXCLUDED.old_refresh_token_hash, rotated_at = EXCLUDED.rotated_at",
        user_id,
        payload.client_uuid,
        refresh_token_hash,
        expiration,
        None::<String>,
        None::<chrono::DateTime<chrono::Utc>>
    ).execute(&state.db_pool).await.map_err(|e| {
        tracing::error!("Failed to upsert session: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if payload.use_cookie.unwrap_or(false) {
        let cookie_header_value = if state.cookie_domain.is_empty() {
            format!(
                "access_token={}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={}",
                access_token, duration_secs
            )
        } else {
            format!(
                "access_token={}; HttpOnly; Secure; SameSite=Lax; Domain={}; Path=/; Max-Age={}",
                access_token, state.cookie_domain, duration_secs
            )
        };

        let browser_response = BrowserAuthResponse {
            user_id,
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

    // 2. Verify token (with 30 seconds grace period for rotated refresh tokens)
    let mut is_valid = false;
    if verify_refresh_token(&session.refresh_token_hash, &payload.refresh_token) {
        if session.expires_at >= chrono::Utc::now() {
            is_valid = true;
        }
    } else if let Some(ref old_hash) = session.old_refresh_token_hash {
        if verify_refresh_token(old_hash, &payload.refresh_token) {
            if let Some(rotated_at) = session.rotated_at {
                let age = chrono::Utc::now() - rotated_at;
                if age <= chrono::Duration::seconds(30) && session.expires_at >= chrono::Utc::now() {
                    is_valid = true;
                }
            }
        }
    }

    if !is_valid {
        // Breach mitigation: Delete all sessions
        tracing::warn!("Breach mitigation: invalidating all sessions for user {}", payload.user_id);
        sqlx::query!("DELETE FROM sessions WHERE user_id = $1", payload.user_id).execute(&state.db_pool).await.ok();
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 3. Rotate tokens
    let duration_secs = payload.expires_in_secs.unwrap_or(86400);
    let duration_secs = if duration_secs <= 0 || duration_secs > 86400 {
        86400
    } else {
        duration_secs
    };

    let access_token = create_access_token(
        &payload.user_id,
        &payload.client_uuid,
        state.jwt_secret.as_bytes(),
        Some(duration_secs),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let new_refresh_token: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();

    let new_hash = hash_refresh_token(&new_refresh_token);
    sqlx::query!(
        "UPDATE sessions
         SET refresh_token_hash = $1, old_refresh_token_hash = $2, rotated_at = $3, expires_at = $4
         WHERE user_id = $5 AND client_uuid = $6",
        new_hash,
        session.refresh_token_hash,
        chrono::Utc::now(),
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
                "access_token={}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={}",
                access_token, duration_secs
            )
        } else {
            format!(
                "access_token={}; HttpOnly; Secure; SameSite=Lax; Domain={}; Path=/; Max-Age={}",
                access_token, state.cookie_domain, duration_secs
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
