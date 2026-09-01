use axum::{
    extract::{Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue},
    response::sse::{Event, KeepAlive, Sse},
    Extension,
};
use futures_util::stream::{self, Stream};
use futures_util::StreamExt;
use serde::Deserialize;
use std::{convert::Infallible, time::Duration};
use uuid::Uuid;

use crate::auth::tokens::Claims;
use crate::state::AppState;
use super::config::fetch_config_snapshot;
use super::publisher::{get_channel_name, get_device_channel_name, SyncSseEvent};
use super::remote_mutations::parse_or_hash_uuid;
use super::types::AppError;

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    pub client_id: Option<String>,
    /// The tablet this stream watches. Config is device-scoped, so without it the stream
    /// only carries account-wide events.
    #[serde(default, alias = "deviceUuid")]
    pub device_uuid: Option<Uuid>,
}

/// Helper function to construct mandatory SSE headers for proxy/Nginx compatibility.
pub fn build_sse_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(header::CONNECTION, HeaderValue::from_static("keep-alive"));
    headers.insert(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    );
    headers
}

/// SSE stream handler for `GET /api/v1/sync/stream` and `GET /api/sync/stream`.
pub async fn sync_stream_handler(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<(HeaderMap, Sse<impl Stream<Item = Result<Event, Infallible>>>), AppError> {
    let user_id = claims.sub.clone();
    let client_id = query.client_id.or_else(|| {
        headers
            .get("x-client-uuid")
            .and_then(|h| h.to_str().ok().map(|s| s.to_string()))
    });

    let device_uuid = query.device_uuid.or_else(|| {
        headers
            .get("x-device-uuid")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| Uuid::parse_str(s).ok())
    });

    // 1. Subscribe to Redis Pub/Sub channels FIRST to prevent race conditions. The
    // account-wide channel always; the device channel too when the caller named a device,
    // which is where config writes for that tablet are published.
    let channel_name = get_channel_name(&user_id);
    let mut pubsub = state.redis_client.get_async_pubsub().await?;
    pubsub.subscribe(&channel_name).await?;
    if let Some(device) = device_uuid {
        pubsub.subscribe(&get_device_channel_name(&user_id, &device)).await?;
    }

    // 2. Query the primary DB for the initial state: the configs this stream is about to
    // receive updates for. Scoped to the named device when there is one, account-wide
    // otherwise — the same split the sync endpoint draws between a tablet and the cloud
    // dashboard. Reading after the subscribe means a write landing in between is replayed
    // as an event rather than lost.
    let user_uuid = parse_or_hash_uuid(&user_id);
    let configs = fetch_config_snapshot(&state.db_pool, &user_uuid, device_uuid).await?;
    let initial_event = SyncSseEvent::InitialState {
        entity: "config".to_string(),
        data: serde_json::to_value(&configs)?,
    };
    let initial_payload = serde_json::to_string(&initial_event)
        .unwrap_or_else(|_| "{}".to_string());

    // Convert Redis PubSub stream into an async message stream
    let pubsub_stream = pubsub.into_on_message();

    // 3 & 4. Flush initial state down SSE stream, then listen to Redis Pub/Sub
    let stream = stream::unfold(
        (Some(initial_payload), pubsub_stream, client_id, device_uuid),
        |(initial, mut pubsub_stream, client_id, device_uuid)| async move {
            if let Some(init_payload) = initial {
                let event = Event::default().event("message").data(init_payload);
                return Some((Ok(event), (None, pubsub_stream, client_id, device_uuid)));
            }

            while let Some(msg) = pubsub_stream.next().await {
                if let Ok(payload_str) = msg.get_payload::<String>() {
                    if let Ok(event) = serde_json::from_str::<SyncSseEvent>(&payload_str) {
                        // Echo filtering logic: skip event if sender matches current client
                        if let Some(ref current_client) = client_id {
                            match &event {
                                SyncSseEvent::DirectUpdate { sender_client_id, .. }
                                | SyncSseEvent::Invalidate { sender_client_id, .. }
                                    if sender_client_id.as_ref() == Some(current_client) =>
                                {
                                    continue;
                                }
                                _ => {}
                            }
                        }
                        // A stream watching one tablet ignores events aimed at another,
                        // which the account-wide channel can still carry.
                        if !event_targets_device(&event, device_uuid) {
                            continue;
                        }
                    }
                    let event = Event::default().event("message").data(payload_str);
                    return Some((Ok(event), (None, pubsub_stream, client_id, device_uuid)));
                }
            }
            None
        },
    );

    // 5. Configure 4-minute auto ping ticker
    let response_headers = build_sse_headers();
    let sse = Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(240))
            .text("ping"),
    );

    Ok((response_headers, sse))
}

/// Whether an event belongs on a stream watching `device_uuid`. An event with no device is
/// account-wide and always passes; a stream with no device only ever sees those.
pub fn event_targets_device(event: &SyncSseEvent, device_uuid: Option<Uuid>) -> bool {
    let event_device = match event {
        SyncSseEvent::DirectUpdate { device_uuid, .. }
        | SyncSseEvent::Invalidate { device_uuid, .. } => *device_uuid,
        SyncSseEvent::InitialState { .. } => None,
    };

    match (event_device, device_uuid) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(target), Some(watching)) => target == watching,
    }
}
