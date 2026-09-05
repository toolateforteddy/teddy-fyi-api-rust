use axum::{extract::{State, Json}, http::{header, StatusCode}, response::{IntoResponse, Response}};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::auth::tokens::{create_access_token, hash_refresh_token, verify_refresh_token};
use rand::RngExt;
use rand::distr::Alphanumeric;

/// Default access-token lifetime, and the ceiling a client may request. Device pairing
/// mints at this length: the tablet has no `expires_in_secs` to ask with.
pub const DEFAULT_SESSION_SECS: i64 = 86400;

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

/// Mints the access/refresh pair for `user_id` and records the session, which is every
/// step of signing in *after* the caller has established who the user is.
///
/// Extracted verbatim from [`login_handler`] so the device-pairing poll handler can mint
/// the identical pair once a parent has claimed a code, without a second copy of the
/// upsert rules drifting away from this one.
pub async fn issue_session(
    state: &AppState,
    user_id: &str,
    email: Option<&str>,
    client_uuid: &str,
    duration_secs: i64,
) -> Result<AuthResponse, StatusCode> {
    // 1. Generate tokens
    let access_token = create_access_token(
        user_id,
        client_uuid,
        state.jwt_secret.as_bytes(),
        Some(duration_secs),
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let refresh_token: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();

    // 2. Upsert user info in users table
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

    // 3. Upsert session
    let refresh_token_hash = hash_refresh_token(&refresh_token);
    let expiration = chrono::Utc::now() + chrono::Duration::days(7);

    sqlx::query!(
        "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (user_id, client_uuid) DO UPDATE
         SET refresh_token_hash = EXCLUDED.refresh_token_hash, expires_at = EXCLUDED.expires_at, old_refresh_token_hash = EXCLUDED.old_refresh_token_hash, rotated_at = EXCLUDED.rotated_at",
        user_id,
        client_uuid,
        refresh_token_hash,
        expiration,
        None::<String>,
        None::<chrono::DateTime<chrono::Utc>>
    ).execute(&state.db_pool).await.map_err(|e| {
        tracing::error!("Failed to upsert session: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(AuthResponse { access_token, refresh_token })
}

pub async fn login_handler(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> Result<Response, StatusCode> {
    // Resolve user ID and email (supporting dev bypass)
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
        (google_payload.sub, google_payload.email.clone())
    };

    let duration_secs = payload.expires_in_secs.unwrap_or(DEFAULT_SESSION_SECS);
    let duration_secs = if duration_secs <= 0 || duration_secs > DEFAULT_SESSION_SECS {
        DEFAULT_SESSION_SECS
    } else {
        duration_secs
    };

    let AuthResponse { access_token, refresh_token } = issue_session(
        &state,
        &user_id,
        email.as_deref(),
        &payload.client_uuid,
        duration_secs,
    )
    .await?;

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

/// Log threshold for consecutive failed refresh attempts against one session.
///
/// Crossing it does not lock the session or delete it -- a lockout would hand the attack
/// this endpoint used to be vulnerable to straight back, since anyone can drive the counter
/// up without a credential. It only escalates the log line, so a session under sustained
/// guessing is loud rather than invisible.
pub const FAILED_REFRESH_ALERT_THRESHOLD: i32 = 10;

/// Exchanges a refresh token for a fresh access/refresh pair, rotating the refresh token.
///
/// This endpoint is unauthenticated: `user_id`, `client_uuid` and the refresh token in the
/// body are everything it gets. That shapes what a *failure* is allowed to do to the stored
/// session, and the rules differ by what the caller actually proved:
///
/// - **Neither hash matches.** The caller presented a token that was never valid, which is
///   evidence of guessing and nothing else. The session is left completely intact and the
///   per-session `failed_refresh_attempts` counter is bumped. Deleting here used to mean
///   that anyone who knew a `user_id` and a `client_uuid` could permanently sign a device
///   out with a single unauthenticated POST containing a garbage token.
/// - **The stored OLD hash matches, inside the 30s grace window.** A retry racing its own
///   rotation. Succeeds, as it always has.
/// - **The stored OLD hash matches, outside the window.** A genuinely issued token is being
///   replayed after it was superseded, which is the reuse signal refresh-token rotation
///   exists to catch. The session is invalidated. This branch is not reachable by guessing:
///   only a real, previously issued token matches the hash, so it cannot be used to log a
///   stranger's device out.
/// - **The stored OLD hash matches but `rotated_at` is NULL.** Same reasoning: a real token
///   was presented, so this is not a guessing attack, but the row cannot tell us whether we
///   are inside the grace window. We keep invalidating -- failing closed on the security
///   axis costs the honest caller one sign-in, while trusting an unbounded-age old token
///   would silently widen the reuse window to forever -- and log at error level, because the
///   NULL is our own data-consistency bug and wants fixing at the source.
/// - **The correct token, on an expired session.** Ordinary cleanup of a dead row. Unchanged.
///
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
        "SELECT user_id, client_uuid, refresh_token_hash, expires_at, created_at, old_refresh_token_hash, rotated_at, failed_refresh_attempts FROM sessions WHERE user_id = $1 AND client_uuid = $2 FOR UPDATE",
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

            // The old hash matched, so a genuinely issued token was presented -- this branch
            // is unreachable by guessing and so is not the remote-logout hole the mismatch
            // branch was. What we cannot tell is the token's age, because the row lost its
            // `rotated_at`. We fail closed and invalidate, since the alternative is honouring
            // an old token of unbounded age; the honest client pays one sign-in. The NULL
            // itself is a bug on our side, hence error rather than warn.
            tracing::error!(
                user_id = %payload.user_id,
                client_uuid = %payload.client_uuid,
                "Data consistency: old refresh token matched but rotated_at is NULL. Cannot prove the token is inside the grace window, so invalidating this session."
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

        // A token that matches neither hash was never issued by us, so the only thing the
        // caller has demonstrated is that they are guessing -- and guessing must not destroy
        // state on an endpoint that takes no credential. The session row stays exactly as it
        // was and the device it belongs to keeps working; all we do is count the attempt, so
        // a real brute-force is boundable and visible rather than silently tolerated.
        //
        // The counter is clamped before the increment so a long-running attack cannot
        // overflow the column, and any successful rotation clears it back to zero.
        let failed_attempts = sqlx::query_scalar!(
            "UPDATE sessions
             SET failed_refresh_attempts = LEAST(failed_refresh_attempts, 1000000) + 1
             WHERE user_id = $1 AND client_uuid = $2
             RETURNING failed_refresh_attempts",
            payload.user_id,
            payload.client_uuid
        )
        .fetch_one(&mut *tx)
        .await
        .unwrap_or(session.failed_refresh_attempts + 1);

        if failed_attempts >= FAILED_REFRESH_ALERT_THRESHOLD {
            tracing::warn!(
                user_id = %payload.user_id,
                client_uuid = %payload.client_uuid,
                failed_refresh_attempts = failed_attempts,
                "Refresh rejected: {} consecutive unrecognised refresh tokens for this session. Session left intact -- an unauthenticated caller must never be able to delete it -- but this looks like brute force.",
                failed_attempts
            );
        } else {
            tracing::warn!(
                user_id = %payload.user_id,
                client_uuid = %payload.client_uuid,
                provided_token_length = payload.refresh_token.len(),
                has_old_hash = session.old_refresh_token_hash.is_some(),
                failed_refresh_attempts = failed_attempts,
                "Refresh rejected: provided refresh token matches neither the current nor the old hash. Session left intact."
            );
        }
        let _ = tx.commit().await;
        return Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "token_mismatch",
                "message": "Provided refresh token does not match current or old hash",
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
    let duration_secs = payload.expires_in_secs.unwrap_or(DEFAULT_SESSION_SECS);
    let duration_secs = if duration_secs <= 0 || duration_secs > DEFAULT_SESSION_SECS {
        DEFAULT_SESSION_SECS
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
        // A successful rotation is proof the legitimate holder is back, so the guess counter
        // starts again from zero: it measures *consecutive* failures, not lifetime ones.
        "UPDATE sessions
         SET refresh_token_hash = $1, old_refresh_token_hash = $2, rotated_at = $3, expires_at = $4, failed_refresh_attempts = 0
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
