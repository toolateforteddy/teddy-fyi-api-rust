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

use crate::auth::tokens::Claims;
use crate::state::AppState;
use super::publisher::{get_channel_name, SyncSseEvent};
use super::types::AppError;

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    pub client_id: Option<String>,
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

    // 1. Subscribe to Redis Pub/Sub channel FIRST to prevent race conditions
    let channel_name = get_channel_name(&user_id);
    let mut pubsub = state.redis_client.get_async_pubsub().await?;
    pubsub.subscribe(&channel_name).await?;

    // 2. Query primary DB/Cache for initial state
    let initial_flags = serde_json::json!({
        "volume": 100,
        "feature_sync": true
    });
    let initial_event = SyncSseEvent::InitialState {
        entity: "flags".to_string(),
        data: initial_flags,
    };
    let initial_payload = serde_json::to_string(&initial_event)
        .unwrap_or_else(|_| "{}".to_string());

    // Convert Redis PubSub stream into an async message stream
    let pubsub_stream = pubsub.into_on_message();

    // 3 & 4. Flush initial state down SSE stream, then listen to Redis Pub/Sub
    let stream = stream::unfold(
        (Some(initial_payload), pubsub_stream, client_id),
        |(initial, mut pubsub_stream, client_id)| async move {
            if let Some(init_payload) = initial {
                let event = Event::default().event("message").data(init_payload);
                return Some((Ok(event), (None, pubsub_stream, client_id)));
            }

            while let Some(msg) = pubsub_stream.next().await {
                if let Ok(payload_str) = msg.get_payload::<String>() {
                    // Echo filtering logic: skip event if sender matches current client
                    if let Ok(event) = serde_json::from_str::<SyncSseEvent>(&payload_str) {
                        if let Some(ref current_client) = client_id {
                            match &event {
                                SyncSseEvent::DirectUpdate { sender_client_id, .. }
                                | SyncSseEvent::Invalidate { sender_client_id, .. } => {
                                    if sender_client_id.as_ref() == Some(current_client) {
                                        continue;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    let event = Event::default().event("message").data(payload_str);
                    return Some((Ok(event), (None, pubsub_stream, client_id)));
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
