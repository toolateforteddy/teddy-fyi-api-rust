use crate::observability::http::{hash_user_id, log_hash_salt_from_env};
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

impl SyncSseEvent {
    /// The variant name, for logs.
    ///
    /// Exists so a log line can say *which kind* of event went out without
    /// `{:?}`-ing the event itself: a `DirectUpdate`'s `Debug` carries `key` and
    /// `value`, and `value` is the config setting a parent just changed. Cloud
    /// Logging is outside the reach of both `DELETE /api/user/data` and
    /// `jobs::reap_stale_users` (see `observability::http`), so anything of the
    /// user's written there is a copy no erasure path can reach.
    pub fn variant(&self) -> &'static str {
        match self {
            SyncSseEvent::DirectUpdate { .. } => "DIRECT_UPDATE",
            SyncSseEvent::Invalidate { .. } => "INVALIDATE",
            SyncSseEvent::InitialState { .. } => "INITIAL_STATE",
        }
    }

    /// Which table the event is about. A fixed vocabulary chosen by this service —
    /// never user text — so it is safe to log and useful to group by.
    pub fn entity(&self) -> &str {
        match self {
            SyncSseEvent::DirectUpdate { entity, .. }
            | SyncSseEvent::Invalidate { entity, .. }
            | SyncSseEvent::InitialState { entity, .. } => entity,
        }
    }
}

/// Which of the two channel shapes an event went out on. Logged instead of the
/// channel string itself, because that string embeds the raw user id.
const CHANNEL_KIND_USER: &str = "user";
const CHANNEL_KIND_DEVICE: &str = "device";

/// Placeholder for the device field on an account-wide publish, matching the
/// `ABSENT` idiom in `observability::http`: every `sse_published` line keeps the
/// same shape so a log-based metric never sees a missing key.
const NO_DEVICE: &str = "-";

/// Emits the one log line a successful publish produces.
///
/// Deliberately takes the event and logs almost nothing from it. The previous line
/// here was `"Published Redis event to channel {}: {:?}"`, which wrote out the
/// whole event on every publish — and a `DirectUpdate`'s `Debug` includes the
/// config `key` and its `value`, i.e. a setting a parent just changed, on the
/// happy path, for every write. Cloud Logging is outside the reach of both
/// `DELETE /api/user/data` and `jobs::reap_stale_users` (the argument is spelled
/// out on `observability::http::LoggedUser`), so that was a copy of user data no
/// erasure path could ever reach. The channel string went the same way: it is
/// `sync_channel:{user_id}`, and the raw user id is the thing `hash_user_id`
/// exists to keep out of the logs.
///
/// What survives is what answers an operational question — did the fan-out happen,
/// on which channel shape, for whose account, about what kind of change — none of
/// which needs the payload. Factored out of both publish functions so the decision
/// lives in one place and a test can assert on the emitted event directly.
pub(crate) fn log_published(
    channel_kind: &str,
    user_id: &str,
    device_uuid: Option<&Uuid>,
    event: &SyncSseEvent,
) {
    let device = device_uuid.map(|id| id.to_string());
    tracing::info!(
        event = "sse_published",
        channel_kind = %channel_kind,
        // Correlatable across lines, not identifying, and erasable-by-salt-rotation
        // in a way the raw id is not.
        user_hash = %hash_user_id(user_id, &log_hash_salt_from_env()),
        // This service's own identifier for a tablet; it carries no user content,
        // and it is what makes a per-device stream debuggable.
        device_uuid = %device.as_deref().unwrap_or(NO_DEVICE),
        sse_event = %event.variant(),
        entity = %event.entity(),
        "published Redis event"
    );
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
    log_published(CHANNEL_KIND_USER, user_id, None, event);
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
    log_published(CHANNEL_KIND_DEVICE, user_id, Some(device_uuid), event);
    Ok(())
}
