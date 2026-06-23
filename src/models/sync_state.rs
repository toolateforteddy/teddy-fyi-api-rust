use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "sync_state", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SyncState {
    Synced,
    PendingInsert,
    PendingUpdate,
    PendingDelete,
}
