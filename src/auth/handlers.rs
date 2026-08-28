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

// See the note on `require_auth`: the `Err` variant is an axum `Response` by contract,
// so there is no boxing fix available here either.
#[allow(clippy::result_large_err)]
pub async fn refresh_handler(
    State(state): State<AppState>,
    Json(payload): Json<RefreshRequest>,
) -> Result<Response, Response> {
    let mut tx = state.db_pool.begin().await.map_err(|e| {
        tracing::error!("Failed to start transaction: {:?}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "database_error",
                "message": "Failed to start database transaction",
                "client_uuid": payload.client_uuid,
                "user_id": payload.user_id,
                "details": { "db_error": format!("{:?}", e) }
            }))
        ).into_response()
    })?;

    // 1. Get session (locked)
    let session = sqlx::query_as!(
        crate::auth::models::Session,
        "SELECT user_id, client_uuid, refresh_token_hash, expires_at, created_at, old_refresh_token_hash, rotated_at FROM sessions WHERE user_id = $1 AND client_uuid = $2 FOR UPDATE",
        payload.user_id,
        payload.client_uuid
    ).fetch_optional(&mut *tx).await.map_err(|e| {
        tracing::error!("Database error during refresh: {:?}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "database_error",
                "message": "Database error during refresh",
                "client_uuid": payload.client_uuid,
                "user_id": payload.user_id,
                "details": { "db_error": format!("{:?}", e) }
            }))
        ).into_response()
    })?;

    let session = match session {
        Some(s) => s,
        None => {
            let active_clients = sqlx::query!(
                "SELECT client_uuid FROM sessions WHERE user_id = $1",
                payload.user_id
            )
            .fetch_all(&mut *tx)
            .await
            .map(|rows| rows.into_iter().map(|r| r.client_uuid).collect::<Vec<_>>())
            .unwrap_or_default();

            tracing::info!(
                user_id = %payload.user_id,
                client_uuid = %payload.client_uuid,
                active_clients = ?active_clients,
                "Refresh failed: No active session found in database"
            );
            let _ = tx.rollback().await;
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "session_not_found",
                    "message": "No active session found in database for this client",
                    "client_uuid": payload.client_uuid,
                    "user_id": payload.user_id,
                    "details": { "active_clients": active_clients }
                }))
            ).into_response());
        }
    };

    // 2. Verify token (with 30 seconds grace period for rotated refresh tokens)
    let is_current = verify_refresh_token(&session.refresh_token_hash, &payload.refresh_token);
    let is_old = session.old_refresh_token_hash.as_ref()
        .map(|old_hash| verify_refresh_token(old_hash, &payload.refresh_token))
        .unwrap_or(false);

    if is_current {
        if session.expires_at < chrono::Utc::now() {
            let active_clients = sqlx::query!(
                "SELECT client_uuid FROM sessions WHERE user_id = $1",
                payload.user_id
            )
            .fetch_all(&mut *tx)
            .await
            .map(|rows| rows.into_iter().map(|r| r.client_uuid).collect::<Vec<_>>())
            .unwrap_or_default();

            tracing::info!(
                user_id = %payload.user_id,
                client_uuid = %payload.client_uuid,
                expires_at = ?session.expires_at,
                "Refresh failed: Session expired. Invalidating single session."
            );
            sqlx::query!(
                "DELETE FROM sessions WHERE user_id = $1 AND client_uuid = $2",
                payload.user_id,
                payload.client_uuid
            )
            .execute(&mut *tx)
            .await
            .ok();
            let _ = tx.commit().await;
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "session_expired",
                    "message": "Session expired",
                    "client_uuid": payload.client_uuid,
                    "user_id": payload.user_id,
                    "details": {
                        "expires_at": session.expires_at,
                        "server_time": chrono::Utc::now(),
                        "active_clients": active_clients
                    }
                }))
            ).into_response());
        }
    } else if is_old {
        if let Some(rotated_at) = session.rotated_at {
            let age = chrono::Utc::now() - rotated_at;
            let age_secs = age.num_seconds();
            if age_secs > 30 {
                let active_clients = sqlx::query!(
                    "SELECT client_uuid FROM sessions WHERE user_id = $1",
                    payload.user_id
                )
                .fetch_all(&mut *tx)
                .await
                .map(|rows| rows.into_iter().map(|r| r.client_uuid).collect::<Vec<_>>())
                .unwrap_or_default();

                tracing::warn!(
                    user_id = %payload.user_id,
                    client_uuid = %payload.client_uuid,
                    rotated_at = ?rotated_at,
                    age_seconds = age_secs,
                    "Breach mitigation: Old refresh token reused outside of 30s grace period. Deleting single session."
                );
                sqlx::query!(
                    "DELETE FROM sessions WHERE user_id = $1 AND client_uuid = $2",
                    payload.user_id,
                    payload.client_uuid
                )
                .execute(&mut *tx)
                .await
                .ok();
                let _ = tx.commit().await;
                return Err((
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": "grace_period_expired",
                        "message": "Refresh token grace period expired (Breach mitigation triggered)",
                        "client_uuid": payload.client_uuid,
                        "user_id": payload.user_id,
                        "details": {
                            "rotated_at": rotated_at,
                            "age_seconds": age_secs,
                            "server_time": chrono::Utc::now(),
                            "active_clients": active_clients
                        }
                    }))
                ).into_response());
            }

            if session.expires_at < chrono::Utc::now() {
                let active_clients = sqlx::query!(
                    "SELECT client_uuid FROM sessions WHERE user_id = $1",
                    payload.user_id
                )
                .fetch_all(&mut *tx)
                .await
                .map(|rows| rows.into_iter().map(|r| r.client_uuid).collect::<Vec<_>>())
                .unwrap_or_default();

                tracing::info!(
                    user_id = %payload.user_id,
                    client_uuid = %payload.client_uuid,
                    expires_at = ?session.expires_at,
                    "Refresh failed: Session expired during old token grace period. Invalidating single session."
                );
                sqlx::query!(
                    "DELETE FROM sessions WHERE user_id = $1 AND client_uuid = $2",
                    payload.user_id,
                    payload.client_uuid
                )
                .execute(&mut *tx)
                .await
                .ok();
                let _ = tx.commit().await;
                return Err((
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": "session_expired_grace_period",
                        "message": "Session expired during old token grace period",
                        "client_uuid": payload.client_uuid,
                        "user_id": payload.user_id,
                        "details": {
                            "expires_at": session.expires_at,
                            "rotated_at": rotated_at,
                            "server_time": chrono::Utc::now(),
                            "active_clients": active_clients
                        }
                    }))
                ).into_response());
            }
        } else {
            let active_clients = sqlx::query!(
                "SELECT client_uuid FROM sessions WHERE user_id = $1",
                payload.user_id
            )
            .fetch_all(&mut *tx)
            .await
            .map(|rows| rows.into_iter().map(|r| r.client_uuid).collect::<Vec<_>>())
            .unwrap_or_default();

            tracing::warn!(
                user_id = %payload.user_id,
                client_uuid = %payload.client_uuid,
                "Breach mitigation: Old refresh token matched but rotated_at is NULL. Deleting single session."
            );
            sqlx::query!(
                "DELETE FROM sessions WHERE user_id = $1 AND client_uuid = $2",
                payload.user_id,
                payload.client_uuid
            )
            .execute(&mut *tx)
            .await
            .ok();
            let _ = tx.commit().await;
            return Err((
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": "rotated_at_null",
                    "message": "Old refresh token matched but rotated_at is NULL (Breach mitigation triggered)",
                    "client_uuid": payload.client_uuid,
                    "user_id": payload.user_id,
                    "details": { "active_clients": active_clients }
                }))
            ).into_response());
        }
    } else {
        let active_clients = sqlx::query!(
            "SELECT client_uuid FROM sessions WHERE user_id = $1",
            payload.user_id
        )
        .fetch_all(&mut *tx)
        .await
        .map(|rows| rows.into_iter().map(|r| r.client_uuid).collect::<Vec<_>>())
        .unwrap_or_default();

        tracing::warn!(
            user_id = %payload.user_id,
            client_uuid = %payload.client_uuid,
            provided_token_length = payload.refresh_token.len(),
            has_old_hash = session.old_refresh_token_hash.is_some(),
            "Breach mitigation: Provided refresh token does not match current or old hash. Deleting single session."
        );
        sqlx::query!(
            "DELETE FROM sessions WHERE user_id = $1 AND client_uuid = $2",
            payload.user_id,
            payload.client_uuid
        )
        .execute(&mut *tx)
        .await
        .ok();
        let _ = tx.commit().await;
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "token_mismatch",
                "message": "Provided refresh token does not match current or old hash (Breach mitigation triggered)",
                "client_uuid": payload.client_uuid,
                "user_id": payload.user_id,
                "details": {
                    "provided_token_length": payload.refresh_token.len(),
                    "has_old_refresh_token_hash": session.old_refresh_token_hash.is_some(),
                    "active_clients": active_clients
                }
            }))
        ).into_response());
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
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "token_generation_error",
                "message": "Failed to generate access token",
                "client_uuid": payload.client_uuid,
                "user_id": payload.user_id,
                "details": { "error": format!("{:?}", e) }
            }))
        ).into_response()
    })?;

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
    ).execute(&mut *tx).await.map_err(|e| {
        tracing::error!("Failed to rotate token: {:?}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "database_error",
                "message": "Failed to rotate token",
                "client_uuid": payload.client_uuid,
                "user_id": payload.user_id,
                "details": { "db_error": format!("{:?}", e) }
            }))
        ).into_response()
    })?;

    tx.commit().await.map_err(|e| {
        tracing::error!("Failed to commit transaction: {:?}", e);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": "database_error",
                "message": "Failed to commit transaction",
                "client_uuid": payload.client_uuid,
                "user_id": payload.user_id,
                "details": { "db_error": format!("{:?}", e) }
            }))
        ).into_response()
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
                .map_err(|e| {
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({
                            "error": "cookie_header_error",
                            "message": "Failed to set access token cookie header",
                            "client_uuid": payload.client_uuid,
                            "user_id": payload.user_id,
                            "details": { "error": format!("{:?}", e) }
                        }))
                    ).into_response()
                })?,
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
