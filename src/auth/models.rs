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
}
