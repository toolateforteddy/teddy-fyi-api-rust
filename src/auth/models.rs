use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use time::OffsetDateTime;

#[derive(Debug, Serialize, Deserialize, FromRow)]
pub struct Session {
    pub user_id: String,
    pub client_uuid: String,
    pub refresh_token_hash: String,
    pub expires_at: OffsetDateTime,
    pub created_at: OffsetDateTime,
}
