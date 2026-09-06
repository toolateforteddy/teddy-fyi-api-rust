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
use super::device::existing_fallback_device;
use super::publisher::{get_channel_name, get_device_channel_name, SyncSseEvent};
use super::remote_mutations::parse_or_hash_uuid;
use super::stream_limits::StreamRefusal;
use super::types::{hash_sync_user, AppError};

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    pub client_id: Option<String>,
    /// The tablet this stream watches. Config is device-scoped, so a stream that names no
    /// device falls back to the account's device rather than going without one — see
    /// `resolve_stream_device`.
    #[serde(default, alias = "deviceUuid")]
    pub device_uuid: Option<Uuid>,
    /// Which app is listening. Kept as a raw string rather than `SyncScope`: clients send
    /// it lowercase (`scope=scribble_keep`), and a strict enum here would reject the very
    /// requests it is meant to classify. Only the cloud app is distinguished — see
    /// `is_cloud_scope`.
    #[serde(default)]
    pub scope: Option<String>,
}

/// Whether the stream belongs to the ScribbleKeep Cloud dashboard.
///
/// The cloud app runs on a machine that is not one of the account's tablets and watches
/// every device at once, so it is the one caller that stays account-wide when it names no
/// device. A tablet in the same position means "my own device" — see `resolve_stream_device`.
fn is_cloud_scope(scope: Option<&str>) -> bool {
    scope
        .map(|s| s.eq_ignore_ascii_case("SCRIBBLE_KEEP_CLOUD"))
        .unwrap_or(false)
}

/// The device an SSE stream watches.
///
/// A stream that names a device watches that one. A tablet that names none falls back to
/// the account's device exactly as its sync requests do, because it is the same tablet on
/// both paths: without this, a client that syncs against the fallback device subscribes to
/// the account-wide channel only and never hears the config writes aimed at it. The cloud
/// dashboard is the exception and stays account-wide.
pub async fn resolve_stream_device(
    pool: &sqlx::PgPool,
    user_uuid: &Uuid,
    requested: Option<Uuid>,
    scope: Option<&str>,
) -> Result<Option<Uuid>, AppError> {
    if let Some(device_uuid) = requested {
        return Ok(Some(device_uuid));
    }
    if is_cloud_scope(scope) {
        return Ok(None);
    }

    let fallback = existing_fallback_device(pool, user_uuid).await?;
    match fallback {
        Some(device) => tracing::debug!(
            user_hash = %hash_sync_user(user_uuid),
            device_uuid = %device,
            "Stream without device_uuid; falling back to the account's device"
        ),
        None => tracing::debug!(
            user_hash = %hash_sync_user(user_uuid),
            "Stream without device_uuid; account has no device yet, staying account-wide"
        ),
    }
    Ok(fallback)
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
    // Same rule as the sync handler: the listening device is the one in the token. The query
    // string and the `X-Client-UUID` header used to be consulted first, and the header is
    // already pinned to the claim by `require_auth`, so in practice this only takes the
    // decision away from `?client_id=`. Here that parameter could only ever hurt its own
    // stream — echo filtering decides which events *this* connection skips — but a caller
    // being able to hand the server a different identity than the one it authenticated with
    // is exactly the shape of bug being fixed on the write path, and there is no client that
    // sends anything but its own id here.
    let client_id = Some(claims.client_uuid.clone());
    if let Some(requested) = query.client_id.as_deref() {
        if requested != claims.client_uuid {
            tracing::warn!(
                token_client_uuid = %claims.client_uuid,
                query_client_id = %requested,
                "Sync stream named a different client than its token; using the token's."
            );
        }
    }

    let requested_device = query.device_uuid.or_else(|| {
        headers
            .get("x-device-uuid")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| Uuid::parse_str(s).ok())
    });

    // 0. Claim a concurrency slot before anything expensive. Streams no longer cost
    // a Redis connection apiece — they share one, see [`super::fanout`] — but they
    // still cost a task, a buffered broadcast receiver and a snapshot query each,
    // and accounts are free, so an uncapped endpoint still lets one account push a
    // replica over. Refusing here, ahead of the config query and the registration,
    // means a refused caller costs neither a database round trip nor a place in the
    // fan-out. The guard is dropped on every exit path below, including the `?`
    // returns, and finally by the stream itself when the client disconnects.
    let stream_slot = state
        .stream_slots
        .try_acquire(&user_id)
        .map_err(|refusal| match refusal {
            StreamRefusal::PerUser => AppError::TooManyRequests(
                "Too many open sync streams for this account".to_string(),
            ),
            StreamRefusal::Global => {
                AppError::Overloaded("Sync stream capacity reached; retry shortly".to_string())
            }
        })?;

    let user_uuid = parse_or_hash_uuid(&user_id);
    let device_uuid =
        resolve_stream_device(&state.db_pool, &user_uuid, requested_device, query.scope.as_deref())
            .await?;

    // 1. Register with the shared subscriber FIRST to prevent race conditions. The
    // account-wide channel always; the device channel too when the stream resolved to a
    // device, which is where config writes for that tablet are published.
    //
    // `SyncFanout::subscribe` returns only once the process-wide connection has
    // actually issued `SUBSCRIBE` to Redis (or has confirmed an existing subscription
    // already covers this channel), so this is the same ordering barrier the old
    // per-stream `pubsub.subscribe` was — see step 2. What has changed is the cost:
    // the listener is a `broadcast` receiver on a connection shared by every stream in
    // the process, not a Redis connection of its own.
    let channel_name = get_channel_name(&user_id);
    let mut listeners = Vec::with_capacity(2);
    listeners.push(state.sync_fanout.subscribe(&channel_name).await?);
    if let Some(device) = device_uuid {
        listeners.push(
            state
                .sync_fanout
                .subscribe(&get_device_channel_name(&user_id, &device))
                .await?,
        );
    }

    // 2. Query the primary DB for the initial state: the configs this stream is about to
    // receive updates for. Scoped to the device the stream resolved to, account-wide
    // when it resolved to none — the same split the sync endpoint draws between a tablet and the cloud
    // dashboard. Reading after the registration means a write landing in between is
    // replayed as an event rather than lost: it is already in this stream's broadcast
    // buffer by the time the snapshot returns, and the loop below drains it.
    let configs = fetch_config_snapshot(&state.db_pool, &user_uuid, device_uuid).await?;
    let initial_event = SyncSseEvent::InitialState {
        entity: "config".to_string(),
        data: serde_json::to_value(&configs)?,
    };
    let initial_payload = serde_json::to_string(&initial_event)
        .unwrap_or_else(|_| "{}".to_string());

    // The two channels' listeners merge into one stream. Each carries its own
    // registry reference count, so both are released together when this stream ends.
    let events = futures_util::stream::select_all(listeners);

    // 3 & 4. Flush initial state down SSE stream, then listen to Redis Pub/Sub
    let stream = stream::unfold(
        (Some(initial_payload), events, client_id, device_uuid),
        |(initial, mut events, client_id, device_uuid)| async move {
            if let Some(init_payload) = initial {
                let event = Event::default().event("message").data(init_payload);
                return Some((Ok(event), (None, events, client_id, device_uuid)));
            }

            while let Some(payload_str) = events.next().await {
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
                return Some((Ok(event), (None, events, client_id, device_uuid)));
            }
            None
        },
    );

    // The gauge and the concurrency slot are held by the stream itself: `map`
    // captures both, so they live exactly as long as the connection and are
    // dropped when the client goes away — which is the only moment a disconnect
    // is actually observable here. Releasing the slot on that same drop is what
    // makes the cap a cap on *concurrent* streams rather than on lifetime opens.
    let connection_guard = crate::observability::metrics::SseConnectionGuard::open();
    let stream = stream.map(move |item| {
        let _guard = &connection_guard;
        let _slot = &stream_slot;
        item
    });

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
