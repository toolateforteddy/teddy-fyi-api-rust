use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use crate::routes::sync::AppError;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SyncSseEvent {
    DirectUpdate {
        entity: String,
        key: String,
        value: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        sender_client_id: Option<String>,
    },
    Invalidate {
        entity: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sender_client_id: Option<String>,
    },
    InitialState {
        entity: String,
        data: serde_json::Value,
    },
}

/// Computes the dedicated Redis Pub/Sub channel for a given user ID.
pub fn get_channel_name(user_id: &str) -> String {
    format!("sync_channel:{}", user_id)
}

/// Publishes an SSE event payload to Redis Pub/Sub for a specific user after DB mutations commit.
pub async fn publish_user_event(
    redis_client: &redis::Client,
    user_id: &str,
    event: &SyncSseEvent,
) -> Result<(), AppError> {
    let payload = serde_json::to_string(event)?;
    let channel = get_channel_name(user_id);

    let mut conn = redis_client.get_multiplexed_async_connection().await?;
    conn.publish::<_, _, ()>(channel, payload).await?;
    tracing::info!("Published Redis event to channel {}: {:?}", channel, event);
    Ok(())
}
