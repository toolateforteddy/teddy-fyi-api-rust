use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
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
        /// The tablet this update is aimed at. Fan-out is still per user on one channel,
        /// so a client with several tablets on the account uses this to ignore events for
        /// the others. Distinct from `sender_client_id`, which suppresses echoes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_uuid: Option<Uuid>,
        /// Set when the update is a tombstone: the row it names was deleted, so a listener
        /// should drop the key rather than store `value`. Absent on ordinary writes.
        #[serde(default, skip_serializing_if = "is_false")]
        is_deleted: bool,
    },
    Invalidate {
        entity: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sender_client_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_uuid: Option<Uuid>,
    },
    InitialState {
        entity: String,
        data: serde_json::Value,
    },
}

fn is_false(value: &bool) -> bool {
    !*value
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
    conn.publish::<_, _, ()>(&channel, payload).await?;
    tracing::info!("Published Redis event to channel {}: {:?}", channel, event);
    Ok(())
}

/// Computes the Redis Pub/Sub channel for one device on an account.
///
/// Config lives per `(user, device)`, so its updates go here rather than on the account-wide
/// channel: a stream that named a device only wants that tablet's writes.
pub fn get_device_channel_name(user_id: &str, device_uuid: &Uuid) -> String {
    format!("sync_channel:{}:device:{}", user_id, device_uuid)
}

/// Publishes an SSE event to a single device's Pub/Sub channel after DB mutations commit.
pub async fn publish_device_event(
    redis_client: &redis::Client,
    user_id: &str,
    device_uuid: &Uuid,
    event: &SyncSseEvent,
) -> Result<(), AppError> {
    let payload = serde_json::to_string(event)?;
    let channel = get_device_channel_name(user_id, device_uuid);

    let mut conn = redis_client.get_multiplexed_async_connection().await?;
    conn.publish::<_, _, ()>(&channel, payload).await?;
    tracing::info!("Published Redis event to channel {}: {:?}", channel, event);
    Ok(())
}
