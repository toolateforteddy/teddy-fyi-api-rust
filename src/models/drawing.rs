use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;
use super::sync_state::SyncState;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow, PartialEq, Eq)]
pub struct Drawing {
    pub id: Uuid,
    pub user_id: Uuid,
    pub client_uuid: Uuid,
    pub version: i32,
    pub is_deleted: bool,
    pub last_modified: i64,
    pub sync_state: SyncState,
    pub created_at: i64,
    pub data: serde_json::Value,
}
