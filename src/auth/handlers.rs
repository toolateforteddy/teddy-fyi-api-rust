use axum::{extract::{State, Json}, http::{header, StatusCode}, response::{IntoResponse, Response}};
use serde::{Deserialize, Serialize};
use crate::state::AppState;
use crate::auth::product::Product;
use crate::auth::tokens::{create_access_token, hash_refresh_token, verify_refresh_token};
use rand::RngExt;
use rand::distr::Alphanumeric;

/// Default access-token lifetime, and the ceiling a client may request. Device pairing
/// mints at this length: the tablet has no `expires_in_secs` to ask with.
///
/// Defined *as* [`crate::auth::tokens::ACCESS_TOKEN_TTL_SECS`] rather than repeating the
/// number, because this constant is only the clamp — `create_access_token` enforces its own
/// ceiling, and when the two were written out separately (both 24 hours, by coincidence
/// rather than by construction) either could have been lowered while the other silently kept
/// minting long tokens. The reasoning for the value itself lives on that constant.
pub const DEFAULT_SESSION_SECS: i64 = crate::auth::tokens::ACCESS_TOKEN_TTL_SECS;

/// How long after a rotation the *previous* refresh token is still accepted.
///
/// Rotation exists so that a stolen refresh token is detectable: the thief and the honest
/// client cannot both keep using the session, and whoever presents a superseded token gets the
/// session destroyed. The grace window is the hole punched in that rule for the honest client
/// that asked twice — it covers the interval between the server committing a rotation and the
/// client durably storing what came back.
///
/// It was 30 seconds, sized for a client that refreshed about once a day. With a 15-minute
/// access token a device refreshes on the order of a hundred times a day, so every way of
/// losing a response in that interval — a request that timed out on the client while the
/// server committed anyway, the app being killed between the HTTP response and the write to
/// encrypted storage, a captive-portal proxy holding a response past the client's own
/// deadline — now gets ~100 more chances to happen per device per day. And the failure is the
/// worst one we have: the retry presents the previous token, lands outside the window, and the
/// session is *deleted*, which shows up to a parent as being signed out for no reason. Making
/// sign-out trustworthy must not make spontaneous sign-out ordinary.
///
/// Two minutes is chosen against the client's own timeouts rather than against a round number:
/// it covers a full HTTP timeout plus a retry plus the process restart in between, which is
/// the longest honest path from "server rotated" to "client asks again with the old token".
/// What it costs is detection latency, and very little of it: an attacker holding a *stolen*
/// token holds the current one, which is not what this window is about, and reuse of a
/// superseded token is still caught — 90 seconds later than before, on a path where the
/// legitimate client's next refresh, minutes away, catches it anyway.
pub const REFRESH_GRACE_SECS: i64 = 120;

/// Builds the `Set-Cookie` value for the session cookie.
///
/// One function for all three call sites (login, refresh, logout) because the `Domain`
/// attribute is the only thing that varies with deployment, and an empty `COOKIE_DOMAIN`
/// is a perfectly ordinary configuration: it means "no Domain attribute", which is what a
/// single-host deployment wants, and the cookie is then host-only. Every other attribute —
/// `HttpOnly`, `Secure`, `SameSite=Lax`, `Path=/` — is identical either way.
///
/// `max_age_secs` is always the access token's own lifetime, so the cookie and the credential
/// inside it die together — now after [`DEFAULT_SESSION_SECS`] rather than a day. A browser
/// session therefore has to refresh roughly every quarter of an hour to keep the cookie alive,
/// which the page already has everything it needs to do: `/auth/login` and `/auth/refresh`
/// return the rotating refresh token in the *body* (`BrowserAuthResponse` /
/// `BrowserRefreshResponse`) precisely so a cookie-mode client can re-mint without one. The
/// `/link` pairing page is unaffected either way — `POST /auth/device/claim` is unauthenticated
/// and validates a Google ID token directly, so it never reads this cookie — but any browser
/// page that assumed a day-long cookie must now call `/auth/refresh` with `use_cookie: true`
/// on a timer or on its first 401.
///
/// This is the *whole* of what `state.cookie_domain` is allowed to influence. It used to
/// double as the switch for the development login bypass; see [`crate::auth::dev_bypass`]
/// for why that coupling was a security bug and where the decision now lives.
pub fn session_cookie(cookie_domain: &str, access_token: &str, max_age_secs: i64) -> String {
    if cookie_domain.is_empty() {
        format!(
            "access_token={}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={}",
            access_token, max_age_secs
        )
    } else {
        format!(
            "access_token={}; HttpOnly; Secure; SameSite=Lax; Domain={}; Path=/; Max-Age={}",
            access_token, cookie_domain, max_age_secs
        )
    }
}

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
    product: Option<Product>,
) -> Result<AuthResponse, StatusCode> {
    // 1. Generate tokens
    let access_token = create_access_token(
        user_id,
        client_uuid,
        product,
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

    // `COALESCE(EXCLUDED.product, sessions.product)` for the same reason `email` uses it
    // above: a later sign-in that cannot name a product -- through an unclassified client
    // ID, or the dev bypass -- must not erase a product this session already knew. The
    // column only ever gains information, never loses it, so classifying a client ID in
    // configuration upgrades its existing sessions on their next sign-in without a
    // backfill.
    let product_wire = product.map(|p| p.as_wire().to_string());
    sqlx::query!(
        "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at, product)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (user_id, client_uuid) DO UPDATE
         SET refresh_token_hash = EXCLUDED.refresh_token_hash, expires_at = EXCLUDED.expires_at, old_refresh_token_hash = EXCLUDED.old_refresh_token_hash, rotated_at = EXCLUDED.rotated_at, product = COALESCE(EXCLUDED.product, sessions.product)",
        user_id,
        client_uuid,
        refresh_token_hash,
        expiration,
        None::<String>,
        None::<chrono::DateTime<chrono::Utc>>,
        product_wire
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
    // Resolve who the caller is. The only way to skip Google validation is the development
    // bypass, and that is a compile-time property of this binary: without the `dev-auth`
    // cargo feature `dev_bypass_identity` is a function that can only return `None`, so the
    // `else` arm is the only reachable path. In particular this decision does not read
    // `state.cookie_domain` — a cookie setting must never be able to switch off
    // authentication. See [`crate::auth::dev_bypass`].
    // The audience is also the only moment this service can tell the two products apart:
    // after this block the token is gone and every session looks alike. So the product
    // travels out of here alongside the identity, into the claim and onto the session row.
    // The dev bypass names no audience and therefore no product, which is the same
    // "unknown" an unclassified client ID produces -- see `crate::auth::product`.
    let (user_id, email, product) = match crate::auth::dev_bypass::dev_bypass_identity(
        &payload.google_auth_token,
        &payload.user_id,
    ) {
        Some((user_id, email)) => (user_id, email, None),
        None => {
            // Verify Google Token (reusing existing google_client)
            let google_payload = state.google_client.validate_id_token(&payload.google_auth_token).await
                .map_err(|_| StatusCode::UNAUTHORIZED)?;

            if !state.client_catalog.is_allowed(&google_payload.aud) {
                tracing::warn!(
                    "Audience mismatch: {} is not a configured client ID",
                    google_payload.aud
                );
                return Err(StatusCode::UNAUTHORIZED);
            }
            let product = state.client_catalog.product_for(&google_payload.aud);
            if product.is_none() {
                tracing::warn!(
                    aud = %google_payload.aud,
                    "Login through a client ID that is not classified per product; this session \
                     gets no product claim and its sync scopes cannot be enforced. Add the ID to \
                     TEDDY_FYI_CLIENT_IDS or SCRIBBLEROUTE_CLIENT_IDS."
                );
            }
            (google_payload.sub, google_payload.email.clone(), product)
        }
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
        product,
    )
    .await?;

    if payload.use_cookie.unwrap_or(false) {
        let cookie_header_value = session_cookie(&state.cookie_domain, &access_token, duration_secs);

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

/// Builds the *only* shape a failing [`refresh_handler`] response is allowed to take:
/// a stable error code and nothing else.
///
/// `POST /auth/refresh` is unauthenticated — the request body is the whole credential,
/// and any caller may post any `user_id` they happen to know. Everything this endpoint
/// answers with is therefore public, so the failure body carries no `details` object, no
/// list of the account's active `client_uuid`s, no `expires_at`/`rotated_at`/`server_time`
/// timestamps, no `provided_token_length`, and no debug-formatted database error. Every
/// one of those is still recorded — unchanged, and in a few places now more completely —
/// by the `tracing` call that immediately precedes each return. They are operator
/// diagnostics; an anonymous caller is not owed them.
///
/// Only two codes survive, because they are the only distinction a client can act on:
///
/// * `unauthorized` (401) — this refresh token will never work again, so the client must
///   send the user back through sign-in.
/// * `database_error` / `internal_error` (500) — the *server* failed, so the refresh token
///   is probably still good and retrying the same request later is worthwhile.
///
/// The previous per-branch codes (`session_not_found`, `session_expired`,
/// `grace_period_expired`, `session_expired_grace_period`, `rotated_at_null`,
/// `token_mismatch`) are deliberately collapsed rather than preserved. Every one of them
/// meant exactly one thing to the client — re-authenticate — so no legitimate client
/// branch is lost; keeping them apart, on the other hand, would hand an anonymous prober
/// an oracle for which `client_uuid`s exist under a given `user_id`, which is the same
/// device-enumeration leak the `active_clients` list was.
fn refresh_error(status: StatusCode, code: &str) -> Response {
    (status, Json(serde_json::json!({ "error": code }))).into_response()
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
/// - **The stored OLD hash matches, inside the [`REFRESH_GRACE_SECS`] grace window.** A retry
///   racing its own rotation. Succeeds, as it always has, and rotates again — the token the
///   racer presented becomes the stored *old* one, so the response the first caller already
///   stored stays valid. (Preserving the original `old` hash instead would strand whichever
///   caller won the race, which is the opposite of what this window is for.)
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
        tracing::error!(
            user_id = %payload.user_id,
            client_uuid = %payload.client_uuid,
            db_error = ?e,
            "Failed to start transaction"
        );
        refresh_error(StatusCode::INTERNAL_SERVER_ERROR, "database_error")
    })?;

    // 1. Get session (locked)
    let session = sqlx::query_as!(
        crate::auth::models::Session,
        "SELECT user_id, client_uuid, refresh_token_hash, expires_at, created_at, old_refresh_token_hash, rotated_at, failed_refresh_attempts, product FROM sessions WHERE user_id = $1 AND client_uuid = $2 FOR UPDATE",
        payload.user_id,
        payload.client_uuid
    ).fetch_optional(&mut *tx).await.map_err(|e| {
        tracing::error!(
            user_id = %payload.user_id,
            client_uuid = %payload.client_uuid,
            db_error = ?e,
            "Database error during refresh"
        );
        refresh_error(StatusCode::INTERNAL_SERVER_ERROR, "database_error")
    })?;

    let session = match session {
        Some(s) => s,
        None => {
            tracing::info!(
                user_id = %payload.user_id,
                client_uuid = %payload.client_uuid,
                "Refresh failed: No active session found in database"
            );
            let _ = tx.rollback().await;
            return Err(refresh_error(StatusCode::UNAUTHORIZED, "unauthorized"));
        }
    };

    // 2. Verify token (with the [`REFRESH_GRACE_SECS`] grace period for rotated refresh tokens)
    let is_current = verify_refresh_token(&session.refresh_token_hash, &payload.refresh_token);
    let is_old = session.old_refresh_token_hash.as_ref()
        .map(|old_hash| verify_refresh_token(old_hash, &payload.refresh_token))
        .unwrap_or(false);

    if is_current {
        if session.expires_at < chrono::Utc::now() {
            tracing::info!(
                user_id = %payload.user_id,
                client_uuid = %payload.client_uuid,
                expires_at = ?session.expires_at,
                server_time = ?chrono::Utc::now(),
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
            return Err(refresh_error(StatusCode::UNAUTHORIZED, "unauthorized"));
        }
    } else if is_old {
        if let Some(rotated_at) = session.rotated_at {
            let age = chrono::Utc::now() - rotated_at;
            let age_secs = age.num_seconds();
            if age_secs > REFRESH_GRACE_SECS {
                tracing::warn!(
                    user_id = %payload.user_id,
                    client_uuid = %payload.client_uuid,
                    rotated_at = ?rotated_at,
                    age_seconds = age_secs,
                    server_time = ?chrono::Utc::now(),
                    grace_seconds = REFRESH_GRACE_SECS,
                    "Breach mitigation: Old refresh token reused outside the rotation grace period. Deleting single session."
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
                return Err(refresh_error(StatusCode::UNAUTHORIZED, "unauthorized"));
            }

            if session.expires_at < chrono::Utc::now() {
                tracing::info!(
                    user_id = %payload.user_id,
                    client_uuid = %payload.client_uuid,
                    expires_at = ?session.expires_at,
                    rotated_at = ?rotated_at,
                    server_time = ?chrono::Utc::now(),
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
                return Err(refresh_error(StatusCode::UNAUTHORIZED, "unauthorized"));
            }
        } else {
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
            return Err(refresh_error(StatusCode::UNAUTHORIZED, "unauthorized"));
        }
    } else {
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
        return Err(refresh_error(StatusCode::UNAUTHORIZED, "unauthorized"));
    }

    // 3. Rotate tokens
    let duration_secs = payload.expires_in_secs.unwrap_or(DEFAULT_SESSION_SECS);
    let duration_secs = if duration_secs <= 0 || duration_secs > DEFAULT_SESSION_SECS {
        DEFAULT_SESSION_SECS
    } else {
        duration_secs
    };

    // Carried from the session row, not from the request: `POST /auth/refresh` is
    // unauthenticated, so anything the body claimed about which product this is would be
    // an attacker-chosen scope grant. A row written before this column existed, or by a
    // sign-in that could not name a product, yields `None` and the refreshed token simply
    // carries no claim -- exactly as the one it replaces did.
    let product = session
        .product
        .as_deref()
        .and_then(Product::from_wire);

    let access_token = create_access_token(
        &payload.user_id,
        &payload.client_uuid,
        product,
        state.jwt_secret.as_bytes(),
        Some(duration_secs),
    )
    .map_err(|e| {
        tracing::error!(
            user_id = %payload.user_id,
            client_uuid = %payload.client_uuid,
            token_error = ?e,
            "Failed to generate access token during refresh"
        );
        refresh_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
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
        tracing::error!(
            user_id = %payload.user_id,
            client_uuid = %payload.client_uuid,
            db_error = ?e,
            "Failed to rotate token"
        );
        refresh_error(StatusCode::INTERNAL_SERVER_ERROR, "database_error")
    })?;

    tx.commit().await.map_err(|e| {
        tracing::error!(
            user_id = %payload.user_id,
            client_uuid = %payload.client_uuid,
            db_error = ?e,
            "Failed to commit transaction"
        );
        refresh_error(StatusCode::INTERNAL_SERVER_ERROR, "database_error")
    })?;

    if payload.use_cookie.unwrap_or(false) {
        let cookie_header_value = session_cookie(&state.cookie_domain, &access_token, duration_secs);

        let browser_response = BrowserRefreshResponse {
            refresh_token: new_refresh_token,
        };

        let mut response = Json(browser_response).into_response();
        response.headers_mut().insert(
            header::SET_COOKIE,
            header::HeaderValue::from_str(&cookie_header_value)
                .map_err(|e| {
                    tracing::error!(
                        user_id = %payload.user_id,
                        client_uuid = %payload.client_uuid,
                        header_error = ?e,
                        "Failed to set access token cookie header"
                    );
                    refresh_error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
                })?,
        );
        Ok(response)
    } else {
        Ok(Json(AuthResponse { access_token, refresh_token: new_refresh_token }).into_response())
    }
}

/// How logout reads the token that names the session to end.
///
/// Deliberately not `Validation::new`, which validates `exp`. An access token that had already
/// expired failed to decode here, so the `DELETE` never ran: the `sessions` row survived, the
/// refresh token inside it stayed good for its remaining seven days, and a parent who had just
/// tapped "sign out" had signed nothing out. At the old 24-hour access token that needed a
/// console left open all day to reach. At `ACCESS_TOKEN_TTL_SECS` it is the ordinary case --
/// leave the app open for a quarter of an hour, tap sign out, and this is the token it presents.
/// That made sign-out least trustworthy exactly when the short TTL was supposed to make it more
/// so.
///
/// **The signature is still verified**, which is the part that carries the security. A caller
/// must still present a token this service minted, naming that `user_id` and `client_uuid`, so
/// this is not the anonymous remote-logout hole that `test_logout_cannot_be_forged_for_another_user`
/// pins shut -- the claims are not attacker-chosen. What it newly admits is a *stale* token
/// ending its own session, and that is the safe direction to err in: the worst it can do is end
/// a session whose holder was entitled to end it anyway, and the alternative is the session that
/// will not die.
///
/// `exp` is still required to be *present* (`Validation::new` puts it in `required_spec_claims`
/// and this leaves that alone); it is only its value that stops being a reason to refuse.
fn logout_validation() -> jsonwebtoken::Validation {
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256);
    validation.validate_exp = false;
    validation
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
            &logout_validation(),
        ) {
            // Delete the session from database
            let _ = sqlx::query!(
                "DELETE FROM sessions WHERE user_id = $1 AND client_uuid = $2",
                token_data.claims.sub,
                token_data.claims.client_uuid
            ).execute(&state.db_pool).await;
        } else {
            // Worth seeing: every one of these is a sign-out that cleared the cookie and left
            // the session alive, which is the failure this function is shaped to avoid.
            tracing::warn!("Logout could not read its token, so no session row was deleted.");
        }
    }

    // 2. Clear cookie. Same builder as the minting paths — an empty value with `Max-Age=0`
    // only clears the cookie if every other attribute, `Domain` included, matches the one
    // that was set, so these must not be allowed to drift apart.
    let cookie_header_value = session_cookie(&state.cookie_domain, "", 0);

    let mut response = StatusCode::OK.into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        header::HeaderValue::from_str(&cookie_header_value)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    Ok(response)
}
