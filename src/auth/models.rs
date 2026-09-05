use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use chrono::{DateTime, Utc};

#[derive(Debug, Serialize, Deserialize, FromRow, Clone)]
pub struct Session {
    pub user_id: String,
    pub client_uuid: String,
    pub refresh_token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub old_refresh_token_hash: Option<String>,
    pub rotated_at: Option<DateTime<Utc>>,
    /// Consecutive failed refresh attempts against this session, reset to zero by every
    /// successful rotation. A failed attempt no longer deletes the session -- see
    /// [`crate::auth::handlers::refresh_handler`] -- so this is what keeps a brute-force
    /// visible instead of silently free.
    pub failed_refresh_attempts: i32,
    /// Which product this session was established for, in
    /// [`crate::auth::product::Product`]'s wire form, or `NULL` when it is not known --
    /// an older session, or one signed in through a client ID that has not been classified
    /// per product yet.
    ///
    /// `refresh_handler` re-mints the access token from this, because the audience that
    /// established the product is long gone by then: `POST /auth/refresh` is
    /// unauthenticated and carries a refresh token and nothing else.
    pub product: Option<String>,
}
