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

use sha2::{Digest, Sha256};

use crate::auth::handlers::{issue_session, AuthResponse};
use crate::auth::product::Product;
use crate::auth::metrics as auth_metrics;
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

/// Where the parent is sent to redeem the code when the caller named no app this service
/// recognises. Configurable so a staging site can point at its own page without a rebuild.
const DEFAULT_VERIFICATION_URI: &str = "https://scribbleroute.com/link";

/// The redemption page each known app's parent is sent to, keyed by the normalised `app`
/// the client sends to [`start_handler`].
///
/// This service is shared: ScribbleRoute's tablets and teddy.fyi's tablets both pair
/// through these endpoints, and they are redeemed on two different websites. A single
/// global URI would send half the parents to a page that does not know their code, so the
/// app that asked for the code decides where it is typed.
///
/// Keys are the wire names the clients send -- `SyncScope`/build-flavour enum names, which
/// are `SCREAMING_SNAKE_CASE` -- run through [`normalize_app`] so spelling drift on the
/// client costs nothing.
const APP_VERIFICATION_URIS: &[(&str, &str)] = &[
    ("SCRIBBLE_KEEP", "https://scribbleroute.com/link"),
    ("SCRIBBLE_BOX", "https://scribbleroute.com/link"),
    ("TEDDY_FYI", "https://teddy.fyi/link"),
    ("TEDDY_FYI_GROCERY", "https://teddy.fyi/link"),
];

/// Outstanding — unexpired, unconsumed — authorizations one `client_uuid` may hold at
/// once. A real tablet needs exactly one: it asks for a code, shows it, and polls until
/// the parent redeems it. Three leaves room for the ways that goes wrong in the field —
/// the app restarted mid-pairing, a request whose response never arrived and was retried —
/// while turning an unauthenticated insert loop into a wall the attacker hits on the
/// fourth call. Without it `/start` mints a row per request forever: the reaper only
/// sweeps rows that have already *expired*, so a sustained insert rate outruns it and the
/// table grows without bound. Expired and consumed rows do not count, so a tablet whose
/// code timed out can immediately ask for another.
const MAX_OUTSTANDING_PER_CLIENT: i64 = 3;

/// Longest `client_uuid` and `app` this endpoint will accept. Both are attacker-chosen
/// strings on an unauthenticated route, and both are written to the database and into log
/// lines, so they are bounded before either happens. A `client_uuid` is a UUID-shaped
/// string (36 characters formatted, 32 bare) and an `app` is a build-flavour enum name
/// like `SCRIBBLE_KEEP`; 128 bytes is far more than either needs and still small enough
/// that no caller can push megabytes through here.
const MAX_CLIENT_UUID_LEN: usize = 128;
const MAX_APP_LEN: usize = 128;

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

/// Folds an `app` as sent into the form [`APP_VERIFICATION_URIS`] and the environment are
/// keyed by: uppercased, with everything that is not a letter or a digit written as `_`.
/// `teddy.fyi grocery` and `TEDDY_FYI_GROCERY` are then the same app, and the result is
/// always a legal environment variable suffix.
fn normalize_app(app: &str) -> String {
    app.trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// A non-empty environment variable, or nothing. A variable set to whitespace is a
/// deployment that meant to unset it.
fn env_uri(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|uri| uri.trim().to_string())
        .filter(|uri| !uri.is_empty())
}

/// Where the parent of `app` redeems their code, most specific source first:
///
/// 1. `DEVICE_VERIFICATION_URI_<APP>` -- one deployment overriding one app, which is how a
///    staging site points a single client at itself.
/// 2. [`APP_VERIFICATION_URIS`] -- what each shipped app expects, with no configuration.
/// 3. `DEVICE_VERIFICATION_URI`, then [`DEFAULT_VERIFICATION_URI`] -- the answer for a
///    caller that named no app, or named one this build has never heard of.
fn verification_uri(app: Option<&str>) -> String {
    if let Some(key) = app.map(normalize_app).filter(|k| !k.is_empty()) {
        if let Some(uri) = env_uri(&format!("DEVICE_VERIFICATION_URI_{key}")) {
            return uri;
        }
        if let Some((_, uri)) = APP_VERIFICATION_URIS.iter().find(|(name, _)| *name == key) {
            return (*uri).to_string();
        }
        // Not an error: an older or newer client may name an app this build predates, and
        // the default page is still a page. Worth a line, because a parent sent to the
        // wrong site is a support question and this is where the answer is.
        tracing::warn!(
            app = %key,
            "Unknown app on device start; falling back to the default verification URI"
        );
    }

    env_uri("DEVICE_VERIFICATION_URI").unwrap_or_else(|| DEFAULT_VERIFICATION_URI.to_string())
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

/// Hashes a `device_code` for storage: a domain-separated SHA-256, hex-encoded.
///
/// Deliberately **not** [`hash_refresh_token`](crate::auth::tokens::hash_refresh_token),
/// which is Argon2id. A refresh token is checked on an authenticated path and lives for
/// months; a device code is 64 characters straight out of a CSPRNG, lives ten minutes, and
/// is presented by anyone who can reach an unauthenticated endpoint. There is no
/// low-entropy, human-chosen part for a memory-hard KDF to defend, so Argon2 here bought
/// nothing and cost two things:
///
/// * `/auth/device/start` burned ~19 MiB and ~50ms of CPU per unauthenticated request — a
///   memory and CPU exhaustion primitive handed to any stranger who could reach it.
/// * An Argon2 digest is *salted*, so it cannot be looked up by value. The poll path had
///   to load every row sharing the caller's `client_uuid` and verify them one at a time,
///   and `client_uuid` is chosen by the caller: seeding thousands of rows under one value
///   turned a single poll into thousands of Argon2 verifications, inside one transaction
///   holding `FOR UPDATE` locks and one of very few pool connections.
///
/// The property that actually mattered — a database dump must not yield a usable code —
/// survives: SHA-256 is not invertible, and 64 alphanumerics is far past any brute force.
/// Being deterministic, the digest is also a direct lookup key on the table's primary key.
///
/// The prefix is domain separation, in the same idiom as
/// [`hash_user_id`](crate::observability::http::hash_user_id), so this digest can never
/// coincide with another SHA-256 this service computes over the same bytes. No secret is
/// mixed in, on purpose: a rotating salt would silently strand every in-flight pairing on
/// the next deployment, and there is nothing to protect here that the code's own 64
/// characters of entropy do not already cover.
fn hash_device_code(device_code: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"teddy-fyi/device-code/v1:");
    hasher.update(device_code.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

/// The audience rule from [`login_handler`](crate::auth::handlers::login_handler), lifted
/// out so it can be exercised without a live Google token.
fn audience_is_allowed(catalog: &crate::auth::client_ids::ClientCatalog, aud: &str) -> bool {
    catalog.is_allowed(aud)
}

/// `POST /auth/device/start` — unauthenticated. Hands the tablet a code to display and a
/// device code to poll with.
/// Over-long input is a `400` before anything is written or logged, and a client holding
/// [`MAX_OUTSTANDING_PER_CLIENT`] live codes is a `429`.
pub async fn start_handler(
    State(state): State<AppState>,
    Json(payload): Json<StartRequest>,
) -> Result<Response, StatusCode> {
    // Length first, and before any `tracing` call: these are unauthenticated,
    // caller-chosen strings, and neither the database nor the log should be the thing
    // that discovers how long they are. Counted in bytes rather than characters because
    // bytes are what the storage and the log line actually cost.
    if payload.client_uuid.len() > MAX_CLIENT_UUID_LEN {
        auth_metrics::record_device_start(auth_metrics::DEVICE_START_INVALID_REQUEST);
        return Err(StatusCode::BAD_REQUEST);
    }
    if payload.app.as_ref().is_some_and(|app| app.len() > MAX_APP_LEN) {
        auth_metrics::record_device_start(auth_metrics::DEVICE_START_INVALID_REQUEST);
        return Err(StatusCode::BAD_REQUEST);
    }

    // Only live rows count: expired ones are the reaper's, and consumed ones belong to a
    // pairing that already finished. A tablet that let a code lapse is not penalised for
    // asking again.
    let outstanding = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!"
             FROM device_authorizations
            WHERE client_uuid = $1
              AND expires_at > now()
              AND consumed_at IS NULL"#,
        payload.client_uuid
    )
    .fetch_one(&state.db_pool)
    .await
    .map_err(|e| {
        tracing::error!("Failed to count outstanding device authorizations: {:?}", e);
        auth_metrics::record_device_start(auth_metrics::DEVICE_START_ERROR);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if outstanding >= MAX_OUTSTANDING_PER_CLIENT {
        tracing::warn!(
            client_uuid = %payload.client_uuid,
            "Device start refused: outstanding authorization cap reached"
        );
        auth_metrics::record_device_start(auth_metrics::DEVICE_START_CAPPED);
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }

    let device_code: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(64)
        .map(char::from)
        .collect();
    let device_code_hash = hash_device_code(&device_code);
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
            auth_metrics::record_device_start(auth_metrics::DEVICE_START_ERROR);
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
            auth_metrics::record_device_start(auth_metrics::DEVICE_START_ERROR);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

        if inserted.is_some() {
            tracing::info!(
                client_uuid = %payload.client_uuid,
                app = ?payload.app,
                "Device authorization started"
            );
            auth_metrics::record_device_start(auth_metrics::DEVICE_START_SUCCESS);
            return Ok(Json(StartResponse {
                device_code,
                user_code: format_user_code(&user_code),
                verification_uri: verification_uri(payload.app.as_deref()),
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
    auth_metrics::record_device_start(auth_metrics::DEVICE_START_ERROR);
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
        .map_err(|_| {
            auth_metrics::record_device_claim(auth_metrics::DEVICE_CLAIM_INVALID_TOKEN);
            StatusCode::UNAUTHORIZED
        })?;

    if !audience_is_allowed(&state.client_catalog, &google_payload.aud) {
        auth_metrics::record_device_claim(auth_metrics::DEVICE_CLAIM_UNKNOWN_AUDIENCE);
        tracing::warn!(
            "Audience mismatch: {} is not a configured client ID",
            google_payload.aud
        );
        return Err(StatusCode::UNAUTHORIZED);
    }

    let user_id = google_payload.sub;
    let email = google_payload.email.clone();
    // The parent redeems the code on their product's own website, so their audience is the
    // one thing in this handshake that *proves* which product the tablet is being paired
    // into. It is recorded on the authorization row here and read back by the poll that
    // mints the session, because the tablet's own request carries no proof of anything.
    let product = state.client_catalog.product_for(&google_payload.aud);
    // See the matching line in `login_handler`: the authorized party is a client
    // identifier, logged rather than labelled, and on this path it is the one thing that
    // names which app the parent redeemed from.
    tracing::info!(
        aud = %google_payload.aud,
        azp = google_payload.azp.as_deref().unwrap_or("<absent>"),
        product = product.map_or("unclassified", Product::as_wire),
        "Device claim audience resolved"
    );

    claim_for_user(&state, &user_id, email.as_deref(), &payload.user_code, product).await
}

/// The half of [`claim_handler`] that runs once the caller's identity is established.
/// Split out so the state machine is testable without a live Google token — and so the
/// only way to reach it from the network is through the verification above.
pub async fn claim_for_user(
    state: &AppState,
    user_id: &str,
    email: Option<&str>,
    raw_user_code: &str,
    product: Option<Product>,
) -> Result<StatusCode, StatusCode> {
    // See `refresh_handler`: the raw subject never reaches the logs, because Cloud Logging
    // is outside the reach of every erasure path this service has.
    let user_hash = crate::observability::http::hash_user_id(
        user_id,
        &crate::observability::http::log_hash_salt(&state.jwt_secret),
    );

    if claim_failures_exhausted(state, user_id).await.inspect_err(|_| {
        auth_metrics::record_device_claim(auth_metrics::DEVICE_CLAIM_ERROR);
    })? {
        tracing::warn!(user_hash = %user_hash, "Device claim rate limit reached");
        auth_metrics::record_device_claim(auth_metrics::DEVICE_CLAIM_RATE_LIMITED);
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
        auth_metrics::record_device_claim(auth_metrics::DEVICE_CLAIM_ERROR);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let claimed = sqlx::query!(
        "UPDATE device_authorizations
            SET user_id = $1, claimed_at = now(), product = $3
          WHERE user_code = $2
            AND expires_at > now()
            AND claimed_at IS NULL
          RETURNING client_uuid",
        user_id,
        user_code,
        product.map(|p| p.as_wire().to_string())
    )
    .fetch_optional(&state.db_pool)
    .await
    .map_err(|e| {
        tracing::error!("Failed to claim device authorization: {:?}", e);
        auth_metrics::record_device_claim(auth_metrics::DEVICE_CLAIM_ERROR);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    match claimed {
        Some(row) => {
            tracing::info!(
                user_hash = %user_hash,
                client_uuid = %row.client_uuid,
                "Device authorization claimed"
            );
            auth_metrics::record_device_claim(auth_metrics::DEVICE_CLAIM_SUCCESS);
            Ok(StatusCode::NO_CONTENT)
        }
        None => {
            record_claim_failure(state, user_id, &user_code).await;
            // Unknown, expired and already-claimed collapse into this one label, exactly as
            // they collapse into one `404` for the caller. Splitting them here would be a
            // second, quieter version of the oracle that response shape exists to deny.
            auth_metrics::record_device_claim(auth_metrics::DEVICE_CLAIM_NOT_FOUND);
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

    tracing::info!(
        user_hash = %crate::observability::http::hash_user_id(
            user_id,
            &crate::observability::http::log_hash_salt(&state.jwt_secret),
        ),
        "Device claim failed"
    );
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
        auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_ERROR);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // One row, found by value on the primary key. [`hash_device_code`] is deterministic,
    // so the digest of the code the tablet presents *is* the stored key — no candidate
    // scan, and nothing the caller sends can widen the work this query does.
    //
    // `client_uuid` stays in the predicate as a real check, not as a narrowing device: it
    // is what stops a device code lifted off one install being replayed by another. A
    // mismatch matches no row and falls out as exactly the `404` an invented code gets, so
    // the response is never an oracle for which codes exist.
    //
    // `FOR UPDATE` locks that single row for the rest of the transaction, which is what
    // keeps two tablets racing one code from both coming away with a session.
    let row = sqlx::query!(
        "SELECT device_code_hash, user_id, expires_at, claimed_at, consumed_at, last_polled_at, product
           FROM device_authorizations
          WHERE device_code_hash = $1 AND client_uuid = $2
          FOR UPDATE",
        hash_device_code(&payload.device_code),
        payload.client_uuid
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| {
        tracing::error!("Failed to load device authorization: {:?}", e);
        auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_ERROR);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let Some(row) = row else {
        let _ = tx.rollback().await;
        auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_NOT_FOUND);
        return Err(StatusCode::NOT_FOUND);
    };

    // Terminal states first: a spent or expired code says so regardless of pacing.
    if row.consumed_at.is_some() || row.expires_at < Utc::now() {
        let _ = tx.rollback().await;
        auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_EXPIRED);
        return Err(StatusCode::GONE);
    }

    if polled_too_soon(row.last_polled_at, Utc::now()) {
        let _ = tx.rollback().await;
        auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_RATE_LIMITED);
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
        auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_ERROR);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Claimed means both halves of the stamp are present; anything else is still pending.
    let user_id = match (row.user_id.clone(), row.claimed_at) {
        (Some(user_id), Some(_)) => user_id,
        _ => {
            tx.commit().await.map_err(|e| {
                tracing::error!("Failed to commit poll: {:?}", e);
                auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_ERROR);
                StatusCode::INTERNAL_SERVER_ERROR
            })?;
            // The healthy steady state, and the highest-volume series here: a tablet polls
            // every few seconds for the whole time the parent is typing the code.
            auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_PENDING);
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
        auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_ERROR);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    if spent.is_none() {
        let _ = tx.rollback().await;
        auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_EXPIRED);
        return Err(StatusCode::GONE);
    }

    tx.commit().await.map_err(|e| {
        tracing::error!("Failed to commit device authorization: {:?}", e);
        auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_ERROR);
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // Whatever the claiming parent proved, and nothing the tablet said about itself. An
    // unclassified parent audience leaves this `None`, which is the ordinary
    // "unknown, so not enforced" case described in `crate::auth::product`.
    let product = row.product.as_deref().and_then(Product::from_wire);

    let auth: AuthResponse = issue_session(
        &state,
        &user_id,
        None,
        &payload.client_uuid,
        crate::auth::handlers::DEFAULT_SESSION_SECS,
        product,
    )
    .await
    .inspect_err(|_| {
        // The code is already spent at this point, deliberately (see above), so this is a
        // pairing the tablet cannot retry -- worth counting as an error rather than
        // letting it vanish into the 500s.
        auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_ERROR);
    })?;
    auth_metrics::record_device_poll(auth_metrics::DEVICE_POLL_AUTHORIZED);

    tracing::info!(
        user_hash = %crate::observability::http::hash_user_id(
            &user_id,
            &crate::observability::http::log_hash_salt(&state.jwt_secret),
        ),
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
