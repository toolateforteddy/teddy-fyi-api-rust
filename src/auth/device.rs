//! Device pairing: signing in where there is no Google identity on the device.
//!
//! Fire OS has no Google Play Services, so the Android client's `androidx.credentials`
//! sign-in cannot run on a Fire tablet — there is no provider on the device to answer it,
//! the client never gets an ID token, and it never reaches `POST /auth/login`. These three
//! endpoints move the Google half of sign-in to a device that *does* have a Google
//! account: the tablet asks for a short code and polls, the parent redeems the code from a
//! browser, and the tablet collects the same access/refresh pair `/auth/login` mints.
//!
//! RFC 8628 (the OAuth device authorization grant) in shape, deliberately — but these are
//! our endpoints minting our own tokens. No Google endpoint is called that
//! [`login_handler`](crate::auth::handlers::login_handler) does not already call.
//!
//! The tablet is the *child's* device. The whole point is that the parent's Google
//! credentials are never typed on it, so nothing here may put a Google flow back on the
//! tablet.
//!
//! Codes are secrets: a `user_code` or a `device_code` is never logged, at any level.

use axum::{
    extract::{Json, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Duration, Utc};
use rand::distr::Alphanumeric;
use rand::RngExt;
use serde::{Deserialize, Serialize};

use crate::auth::handlers::{issue_session, AuthResponse};
use crate::auth::tokens::{hash_refresh_token, verify_refresh_token};
use crate::state::AppState;

/// Characters a `user_code` is drawn from, written out in the order the spec gives them
/// (`context/2026-09-04_device_pairing_auth.md`, step 3) because the website's entry field
/// has to agree with this exactly. The 36 alphanumerics minus four groups: every vowel, so
/// a code can never spell a word a parent has to read aloud; `0`/`1`/`I`/`L`, the classic
/// lookalikes; and `S`/`Z`/`B`/`G`, which read as `5`/`2`/`8`/`6` — the letter goes and the
/// digit stays, so each ambiguous pair keeps exactly one legal member.
///
/// 24 symbols, so an 8-character code is a ~24^8 (≈1.1e11, about 36 bits) space. That is
/// not what stops guessing — the ten-minute lifetime, the single use and
/// [`MAX_CLAIM_FAILURES`] are — it only means the length is not the weak part.
const USER_CODE_ALPHABET: &[u8] = b"23456789CDFHJKMNPQRTVWXY";

/// Characters in a `user_code`, displayed as two groups of four.
const USER_CODE_LEN: usize = 8;

/// How long a code is good for, and the `expires_in` handed to the tablet.
const CODE_TTL_SECS: i64 = 600;

/// Minimum seconds between polls of one code, and the `interval` handed to the tablet.
const POLL_INTERVAL_SECS: i64 = 5;

/// Failed claims, within [`CLAIM_FAILURE_WINDOW_MINS`], that lock a Google account out of
/// claiming. The code space is large and a code lives ten minutes, but guessing must still
/// cost something.
const MAX_CLAIM_FAILURES: i64 = 5;
const CLAIM_FAILURE_WINDOW_MINS: i64 = 10;

/// Where the parent is sent to redeem the code. Configurable so a staging site can point at
/// its own page without a rebuild.
const DEFAULT_VERIFICATION_URI: &str = "https://scribbleroute.com/link";

/// Attempts to land a unique `user_code` before giving up. Each attempt is a fresh draw
/// from a space of ~1.1e11, so more than one is already vanishingly unlikely.
const CODE_GENERATION_ATTEMPTS: usize = 8;

#[derive(Deserialize)]
pub struct StartRequest {
    pub client_uuid: String,
    /// Which app asked. Recorded for diagnostics; it is not part of any check.
    pub app: Option<String>,
}

#[derive(Serialize)]
pub struct StartResponse {
    pub device_code: String,
    /// Formatted `XXXX-XXXX` for display. The dash is presentation only — [`claim_handler`]
    /// accepts the code with or without it.
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: i64,
    pub interval: i64,
}

#[derive(Deserialize)]
pub struct ClaimRequest {
    #[serde(alias = "id_token")]
    pub google_auth_token: String,
    pub user_code: String,
}

#[derive(Deserialize)]
pub struct PollRequest {
    pub device_code: String,
    pub client_uuid: String,
}

fn verification_uri() -> String {
    std::env::var("DEVICE_VERIFICATION_URI")
        .ok()
        .filter(|uri| !uri.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_VERIFICATION_URI.to_string())
}

/// Draws a `user_code` from [`USER_CODE_ALPHABET`]. Stored and compared without the
/// display dash; see [`format_user_code`].
fn generate_user_code() -> String {
    let mut rng = rand::rng();
    (0..USER_CODE_LEN)
        .map(|_| {
            let index = rng.random_range(0..USER_CODE_ALPHABET.len());
            USER_CODE_ALPHABET[index] as char
        })
        .collect()
}

/// `CDFHJKMN` → `CDFH-JKMN`. Presentation only.
pub fn format_user_code(code: &str) -> String {
    if code.len() != USER_CODE_LEN {
        return code.to_string();
    }
    format!("{}-{}", &code[..4], &code[4..])
}

/// Folds what a parent actually types back to the stored form: uppercased, with the
/// display dash and any stray spacing removed.
pub fn normalize_user_code(input: &str) -> String {
    input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// The audience rule from [`login_handler`](crate::auth::handlers::login_handler), lifted
/// out so it can be exercised without a live Google token.
fn audience_is_allowed(allowed: &std::collections::HashSet<String>, aud: &str) -> bool {
    allowed.contains(aud)
}

/// `POST /auth/device/start` — unauthenticated. Hands the tablet a code to display and a
/// device code to poll with.
pub async fn start_handler(
    State(state): State<AppState>,
    Json(payload): Json<StartRequest>,
) -> Result<Response, StatusCode> {
    let device_code: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();
    let device_code_hash = hash_refresh_token(&device_code);
    let expires_at = Utc::now() + Duration::seconds(CODE_TTL_SECS);

    for _ in 0..CODE_GENERATION_ATTEMPTS {
        let user_code = generate_user_code();

        // An expired row still holds the unique index, so retire it before treating the
        // draw as a collision.
        sqlx::query!(
            "DELETE FROM device_authorizations WHERE user_code = $1 AND expires_at < now()",
            user_code
        )
        .execute(&state.db_pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to retire expired device authorization: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        let inserted = sqlx::query!(
            "INSERT INTO device_authorizations
                 (device_code_hash, user_code, client_uuid, app, expires_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT DO NOTHING
             RETURNING user_code",
            device_code_hash,
            user_code,
            payload.client_uuid,
            payload.app,
            expires_at
        )
        .fetch_optional(&state.db_pool)
        .await
        .map_err(|e| {
            tracing::error!("Failed to create device authorization: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        if inserted.is_some() {
            tracing::info!(
                client_uuid = %payload.client_uuid,
                app = ?payload.app,
                "Device authorization started"
            );
            return Ok(Json(StartResponse {
                device_code,
                user_code: format_user_code(&user_code),
                verification_uri: verification_uri(),
                expires_in: CODE_TTL_SECS,
                interval: POLL_INTERVAL_SECS,
            })
            .into_response());
        }
    }

    tracing::error!(
        client_uuid = %payload.client_uuid,
        "Exhausted user code generation attempts"
    );
    Err(StatusCode::INTERNAL_SERVER_ERROR)
}

/// `POST /auth/device/claim` — unauthenticated, called from the browser at the
/// verification page. Ties the parent's Google account to the code they typed.
///
/// `204` on success. Anything unknown, expired or already claimed is a `404`, so the
/// response cannot be used to sort real codes from invented ones.
pub async fn claim_handler(
    State(state): State<AppState>,
    Json(payload): Json<ClaimRequest>,
) -> Result<StatusCode, StatusCode> {
    // Verify the Google ID token exactly as `login_handler` does. Deliberately without
    // that handler's `mock.` dev bypass: this endpoint is the one place a stranger's
    // browser can reach a *claim*, and it gets no shortcut.
    let google_payload = state
        .google_client
        .validate_id_token(&payload.google_auth_token)
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?;

    if !audience_is_allowed(&state.google_client_ids, &google_payload.aud) {
        tracing::warn!(
            "Audience mismatch: expected one of {:?}, got {}",
            state.google_client_ids,
            google_payload.aud
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

    let user_id = google_payload.sub;
    let email = google_payload.email.clone();

    claim_for_user(&state, &user_id, email.as_deref(), &payload.user_code).await
}

/// The half of [`claim_handler`] that runs once the caller's identity is established.
/// Split out so the state machine is testable without a live Google token — and so the
/// only way to reach it from the network is through the verification above.
pub async fn claim_for_user(
    state: &AppState,
    user_id: &str,
    email: Option<&str>,
    raw_user_code: &str,
) -> Result<StatusCode, StatusCode> {
    if claim_failures_exhausted(state, user_id).await? {
        tracing::warn!(user_id = %user_id, "Device claim rate limit reached");
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    let user_code = normalize_user_code(raw_user_code);

    // The parent may not have an account yet — the tablet has never signed in — so the
    // `users` row is created here rather than at poll time.
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

    let claimed = sqlx::query!(
        "UPDATE device_authorizations
            SET user_id = $1, claimed_at = now()
          WHERE user_code = $2
            AND expires_at > now()
            AND claimed_at IS NULL
          RETURNING client_uuid",
        user_id,
        user_code
    )
    .fetch_optional(&state.db_pool)
    .await
    .map_err(|e| {
        tracing::error!("Failed to claim device authorization: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    match claimed {
        Some(row) => {
            tracing::info!(
                user_id = %user_id,
                client_uuid = %row.client_uuid,
                "Device authorization claimed"
            );
            Ok(StatusCode::NO_CONTENT)
        }
        None => {
            record_claim_failure(state, user_id, &user_code).await;
            Err(StatusCode::NOT_FOUND)
        }
    }
}

/// True once this Google account has failed [`MAX_CLAIM_FAILURES`] claims inside the
/// window. Counted per account rather than per row, because a mistyped code matches no row
/// at all — which is exactly the case worth limiting.
async fn claim_failures_exhausted(state: &AppState, user_id: &str) -> Result<bool, StatusCode> {
    let since = Utc::now() - Duration::minutes(CLAIM_FAILURE_WINDOW_MINS);
    let failures = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!"
             FROM device_claim_failures
            WHERE user_id = $1 AND failed_at > $2"#,
        user_id,
        since
    )
    .fetch_one(&state.db_pool)
    .await
    .map_err(|e| {
        tracing::error!("Failed to read device claim failures: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    Ok(failures >= MAX_CLAIM_FAILURES)
}

/// Records a failed claim against the account, and against the row if the code named one
/// that merely could not be claimed. Best-effort: a bookkeeping failure must not turn a
/// `404` into a `500` and hand the caller a signal.
async fn record_claim_failure(state: &AppState, user_id: &str, user_code: &str) {
    if let Err(e) = sqlx::query!(
        "INSERT INTO device_claim_failures (user_id) VALUES ($1)",
        user_id
    )
    .execute(&state.db_pool)
    .await
    {
        tracing::error!("Failed to record device claim failure: {:?}", e);
    }

    if let Err(e) = sqlx::query!(
        "UPDATE device_authorizations SET attempts = attempts + 1 WHERE user_code = $1",
        user_code
    )
    .execute(&state.db_pool)
    .await
    {
        tracing::error!("Failed to increment device authorization attempts: {:?}", e);
    }

    tracing::info!(user_id = %user_id, "Device claim failed");
}

/// `POST /auth/device/poll` — unauthenticated. The tablet's side of the handshake.
///
/// | Condition | Response |
/// | :-- | :-- |
/// | Unclaimed, unexpired | `202` `{"status":"pending"}` |
/// | Claimed | `200` + `AuthResponse`, and the code is spent |
/// | Expired, or already consumed | `410` |
/// | Polled faster than the advertised interval | `429` |
/// | `client_uuid` does not match `/start` | `404`, the same shape as an unknown code |
pub async fn poll_handler(
    State(state): State<AppState>,
    Json(payload): Json<PollRequest>,
) -> Result<Response, StatusCode> {
    let mut tx = state.db_pool.begin().await.map_err(|e| {
        tracing::error!("Failed to start transaction: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // `device_code_hash` is a salted Argon2 digest, so it cannot be looked up by value.
    // Narrowing by `client_uuid` first is what makes the scan small — and it is the same
    // check that keeps a leaked device code from being replayed by another install, so a
    // mismatch simply finds no candidate and falls out as a `404`.
    let candidates = sqlx::query!(
        "SELECT device_code_hash, user_id, expires_at, claimed_at, consumed_at, last_polled_at
           FROM device_authorizations
          WHERE client_uuid = $1
          FOR UPDATE",
        payload.client_uuid
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("Failed to load device authorizations: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let Some(row) = candidates
        .into_iter()
        .find(|row| verify_refresh_token(&row.device_code_hash, &payload.device_code))
    else {
        let _ = tx.rollback().await;
        return Err(StatusCode::NOT_FOUND);
    };

    // Terminal states first: a spent or expired code says so regardless of pacing.
    if row.consumed_at.is_some() || row.expires_at < Utc::now() {
        let _ = tx.rollback().await;
        return Err(StatusCode::GONE);
    }

    if polled_too_soon(row.last_polled_at, Utc::now()) {
        let _ = tx.rollback().await;
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    sqlx::query!(
        "UPDATE device_authorizations SET last_polled_at = now() WHERE device_code_hash = $1",
        row.device_code_hash
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("Failed to record poll: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Claimed means both halves of the stamp are present; anything else is still pending.
    let user_id = match (row.user_id.clone(), row.claimed_at) {
        (Some(user_id), Some(_)) => user_id,
        _ => {
            tx.commit().await.map_err(|e| {
                tracing::error!("Failed to commit poll: {:?}", e);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
            return Ok((
                StatusCode::ACCEPTED,
                Json(serde_json::json!({ "status": "pending" })),
            )
                .into_response());
        }
    };

    // Spend the code inside the transaction that read it, and only if it is still unspent:
    // two tablets racing the same code cannot both come away with a session. Minting
    // happens after the commit, so a failure there burns the code rather than leaving a
    // replayable one behind.
    let spent = sqlx::query!(
        "UPDATE device_authorizations
            SET consumed_at = now()
          WHERE device_code_hash = $1 AND consumed_at IS NULL
          RETURNING device_code_hash",
        row.device_code_hash
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("Failed to consume device authorization: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if spent.is_none() {
        let _ = tx.rollback().await;
        return Err(StatusCode::GONE);
    }

    tx.commit().await.map_err(|e| {
        tracing::error!("Failed to commit device authorization: {:?}", e);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let auth: AuthResponse = issue_session(
        &state,
        &user_id,
        None,
        &payload.client_uuid,
        crate::auth::handlers::DEFAULT_SESSION_SECS,
    )
    .await?;

    tracing::info!(
        user_id = %user_id,
        client_uuid = %payload.client_uuid,
        "Device authorization consumed"
    );
    Ok(Json(auth).into_response())
}

/// Whether this poll landed inside the interval the tablet was told to wait. A code that
/// has never been polled is always in time.
fn polled_too_soon(last_polled_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    last_polled_at
        .map(|last| now - last < Duration::seconds(POLL_INTERVAL_SECS))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests;
