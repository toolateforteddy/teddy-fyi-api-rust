//! One shared Redis Pub/Sub connection per process, fanned out in-process.
//!
//! # Why this exists
//!
//! A Redis connection that has issued `SUBSCRIBE` can no longer serve ordinary
//! commands, so it cannot come from a pool: it is pinned for as long as the
//! subscription lives. The SSE handler used to take one such connection *per open
//! stream*, which made a free-to-create HTTP connection cost a scarce, globally
//! shared Redis file descriptor — and Redis `maxclients` is shared by every replica
//! and by the ordinary command pool, so exhausting it takes the whole service out
//! of rotation (`/healthz/ready` pings Redis).
//!
//! This module removes that coupling. The process holds **one** pub/sub connection,
//! owned by a background task; every stream registers with an in-process
//! [`tokio::sync::broadcast`] channel instead. A thousand streams now cost one
//! Redis connection rather than a thousand.
//!
//! The concurrency caps in [`crate::routes::sync::stream_limits`] stay: they now
//! bound memory, tasks and snapshot queries rather than Redis connections. Removing
//! the scarcity is not a reason to remove the ceiling.
//!
//! # Per-channel subscribe, not a pattern subscribe
//!
//! Channels are per user (`sync_channel:{user}`) and per device
//! (`sync_channel:{user}:device:{uuid}`), so the shared connection has to track
//! which of them anybody is listening to. The alternative — one
//! `PSUBSCRIBE sync_channel:*` and filter locally — is simpler to write and was
//! rejected: it makes *every* replica receive *every* account's sync traffic,
//! turning a per-user fan-out into a broadcast whose cost grows with total write
//! volume times replica count, and moves the routing work Redis already does into
//! our own process. Per-channel `SUBSCRIBE`/`UNSUBSCRIBE` keeps Redis sending a
//! replica only what it has a listener for; the price is the reference counting
//! below, which is cheap and bounded.
//!
//! Refcounts are what keep this from re-growing the leak in a new place: the entry
//! for a channel is removed and `UNSUBSCRIBE` is issued the moment its last
//! listener drops, so a user who disconnects leaves nothing behind — neither a map
//! entry nor a Redis-side subscription.
//!
//! # Ordering, and the race the SSE handler depends on
//!
//! The handler must be registered *before* it reads its config snapshot from
//! Postgres, so that a write landing in between is delivered as an event instead of
//! being lost between the two. [`SyncFanout::subscribe`] preserves that: it inserts
//! the broadcast channel synchronously, then **awaits an acknowledgement** that the
//! manager task has actually issued `SUBSCRIBE` to Redis before it returns. Only
//! then does the handler run its snapshot query. Events that arrive in the gap are
//! held in the broadcast channel's buffer, which the listener drains after the
//! snapshot — the same guarantee the old per-connection subscribe gave, for the
//! same reason.
//!
//! Subscribe and unsubscribe both travel down one FIFO command queue, so a
//! `SUBSCRIBE` issued by a new listener can never be overtaken by the
//! `UNSUBSCRIBE` of the listener it replaced.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use futures_util::Stream;
use futures_util::StreamExt;
use redis::aio::{PubSubSink, PubSubStream};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;
use tokio_stream::wrappers::BroadcastStream;

use super::types::AppError;

/// Events buffered per channel before a slow listener starts losing them.
///
/// Deliberately finite. A client that cannot keep up must not be able to stall the
/// one connection everybody shares, so it loses events instead — and a lagging
/// listener recovers on its next `POST /api/sync`, which reconciles by version
/// rather than by event. Sized for a burst of a sync batch's worth of writes.
const CHANNEL_BUFFER: usize = 256;

/// First reconnect delay after the shared connection drops.
const RECONNECT_INITIAL_DELAY: Duration = Duration::from_millis(250);
/// Ceiling for the reconnect backoff. Redis being down is an outage we retry out
/// of, not one we hammer through.
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(10);

/// What the manager task is asked to do. Both variants go down one queue so their
/// order at Redis matches the order listeners registered and left in.
enum Command {
    /// Issue `SUBSCRIBE` and confirm it, so the caller knows Redis is listening
    /// before it reads its snapshot.
    Subscribe {
        channel: String,
        ack: oneshot::Sender<Result<(), String>>,
    },
    /// Issue `UNSUBSCRIBE`. Fire-and-forget: it is sent from a `Drop`, which cannot
    /// await, and nothing depends on when it lands.
    Unsubscribe { channel: String },
}

/// One channel's in-process fan-out plus its listener count.
struct ChannelState {
    sender: broadcast::Sender<String>,
    listeners: usize,
}

/// Channels this process currently has a listener for. Only those: an entry is
/// created by the first listener and removed by the last, so the map cannot grow
/// with users who have disconnected.
#[derive(Default)]
struct Registry {
    channels: HashMap<String, ChannelState>,
}

/// The process-wide shared subscriber.
///
/// Held in `AppState` behind an `Arc`; the single background task it spawns owns
/// the Redis connection for the lifetime of the process.
pub struct SyncFanout {
    registry: Arc<Mutex<Registry>>,
    commands: mpsc::UnboundedSender<Command>,
}

impl SyncFanout {
    /// Starts the shared subscriber. Requires a Tokio runtime, and never fails:
    /// Redis being unreachable at startup is an outage to reconnect out of, not a
    /// reason to refuse to boot — the same stance `init_app_state` already takes by
    /// not dialling Redis at all.
    pub fn spawn(client: redis::Client) -> Arc<Self> {
        let registry = Arc::new(Mutex::new(Registry::default()));
        let (commands, receiver) = mpsc::unbounded_channel();

        tokio::spawn(run_shared_subscriber(
            client,
            receiver,
            Arc::clone(&registry),
        ));

        Arc::new(Self { registry, commands })
    }

    /// Registers a listener for `channel`, subscribing the shared connection if
    /// this is the first one.
    ///
    /// Returns only once Redis has acknowledged the `SUBSCRIBE`, which is what lets
    /// the SSE handler read its snapshot afterwards without a gap — see the module
    /// docs. When the channel already has listeners no Redis round trip happens at
    /// all: the existing subscription already covers this one.
    pub async fn subscribe(&self, channel: &str) -> Result<ChannelListener, AppError> {
        let (receiver, first_listener) = {
            let mut registry = self.lock();
            match registry.channels.get_mut(channel) {
                Some(state) => {
                    state.listeners += 1;
                    (state.sender.subscribe(), false)
                }
                None => {
                    let (sender, receiver) = broadcast::channel(CHANNEL_BUFFER);
                    registry.channels.insert(
                        channel.to_string(),
                        ChannelState {
                            sender,
                            listeners: 1,
                        },
                    );
                    (receiver, true)
                }
            }
        };

        // Built before the await deliberately: every early return below drops it,
        // which releases the reference count and — if this was the first listener —
        // queues the matching `UNSUBSCRIBE`. No failure path can leak a half-made
        // subscription.
        let listener = ChannelListener {
            stream: BroadcastStream::new(receiver),
            _guard: SubscriptionGuard {
                registry: Arc::clone(&self.registry),
                commands: self.commands.clone(),
                channel: channel.to_string(),
            },
        };

        if first_listener {
            let (ack, acked) = oneshot::channel();
            self.commands
                .send(Command::Subscribe {
                    channel: channel.to_string(),
                    ack,
                })
                .map_err(|_| {
                    AppError::Internal("Redis fan-out task is not running".to_string())
                })?;

            match acked.await {
                Ok(Ok(())) => {}
                Ok(Err(err)) => {
                    return Err(AppError::Internal(format!(
                        "Failed to subscribe to Redis channel {channel}: {err}"
                    )))
                }
                Err(_) => {
                    return Err(AppError::Internal(
                        "Redis fan-out task stopped before confirming the subscription".to_string(),
                    ))
                }
            }
        }

        Ok(listener)
    }

    /// Listeners currently registered for `channel`. Diagnostics and tests.
    pub fn listener_count(&self, channel: &str) -> usize {
        self.lock()
            .channels
            .get(channel)
            .map(|state| state.listeners)
            .unwrap_or(0)
    }

    /// Channels this process holds a Redis subscription for. Diagnostics and tests.
    pub fn subscribed_channels(&self) -> usize {
        self.lock().channels.len()
    }

    /// A poisoned lock is recovered from rather than propagated, for the same
    /// reason as in [`crate::routes::sync::stream_limits`]: the only mutations here
    /// are a counter and a map entry, and refusing every stream forever because
    /// some unrelated task panicked is the worse outage.
    fn lock(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Holds one listener's place in the registry for as long as it is alive, exactly
/// as [`crate::routes::sync::stream_limits::StreamSlot`] holds its concurrency
/// slot: an SSE stream ends when the client vanishes, and only a destructor
/// reliably observes that.
struct SubscriptionGuard {
    registry: Arc<Mutex<Registry>>,
    commands: mpsc::UnboundedSender<Command>,
    channel: String,
}

impl Drop for SubscriptionGuard {
    fn drop(&mut self) {
        let mut registry = self
            .registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let last_listener = match registry.channels.get_mut(&self.channel) {
            Some(state) => {
                state.listeners = state.listeners.saturating_sub(1);
                state.listeners == 0
            }
            // The connection dropped and the manager cleared the registry while this
            // listener was alive. Nothing to release, and nothing to unsubscribe:
            // the socket that held the subscription is already gone.
            None => false,
        };

        if last_listener {
            registry.channels.remove(&self.channel);
            drop(registry);
            let _ = self.commands.send(Command::Unsubscribe {
                channel: self.channel.clone(),
            });
        }
    }
}

/// One stream's view of a channel. Yields raw payloads; holds the registry
/// reference count for as long as the caller keeps it.
///
/// Ends when the shared connection drops (the manager clears the registry, which
/// drops the broadcast sender). That is deliberate: it ends the SSE response, the
/// client reconnects, and the reconnect replays a fresh snapshot — the same
/// recovery the old per-stream connection got for free when its own socket died.
pub struct ChannelListener {
    stream: BroadcastStream<String>,
    _guard: SubscriptionGuard,
}

impl Stream for ChannelListener {
    type Item = String;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match futures_util::ready!(Pin::new(&mut this.stream).poll_next(cx)) {
                Some(Ok(payload)) => return Poll::Ready(Some(payload)),
                // A listener too slow for `CHANNEL_BUFFER` loses the overflow rather
                // than back-pressuring the shared connection. Keep the stream open:
                // the client reconciles on its next sync.
                Some(Err(BroadcastStreamRecvError::Lagged(missed))) => {
                    tracing::warn!(
                        missed,
                        "SSE listener fell behind the shared Redis fan-out; dropped events"
                    );
                    metrics::counter!("sse_fanout_lagged_events_total").increment(missed);
                    continue;
                }
                None => return Poll::Ready(None),
            }
        }
    }
}

/// Pushes a payload into the broadcast channel for `channel`, if anybody is
/// listening. A send error only means every listener dropped in the meantime.
fn deliver_to_listeners(registry: &Arc<Mutex<Registry>>, channel: &str, payload: String) {
    let registry = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(state) = registry.channels.get(channel) {
        let _ = state.sender.send(payload);
    }
}

/// Why [`serve`] returned.
enum ServeExit {
    /// The Redis connection ended or a command failed on it. Reconnect.
    ConnectionLost,
    /// Every [`SyncFanout`] handle is gone, so the process is shutting down.
    ShuttingDown,
}

/// The manager task: owns the one pub/sub connection, and reconnects forever.
async fn run_shared_subscriber(
    client: redis::Client,
    mut commands: mpsc::UnboundedReceiver<Command>,
    registry: Arc<Mutex<Registry>>,
) {
    let mut delay = RECONNECT_INITIAL_DELAY;

    loop {
        match client.get_async_pubsub().await {
            Ok(pubsub) => {
                let (mut sink, stream) = pubsub.split();
                // Anything registered while the connection was down (a subscribe
                // queued mid-outage) is restored before we start serving, so no
                // listener is left silently unsubscribed.
                match resubscribe_all(&mut sink, &registry).await {
                    Ok(()) => {
                        delay = RECONNECT_INITIAL_DELAY;
                        tracing::info!("Shared Redis pub/sub connection established");
                        match serve(&mut sink, stream, &mut commands, &registry).await {
                            ServeExit::ShuttingDown => return,
                            ServeExit::ConnectionLost => {
                                tracing::warn!(
                                    "Shared Redis pub/sub connection lost; reconnecting"
                                );
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!("Failed to restore Redis subscriptions: {:?}", err);
                    }
                }
            }
            Err(err) => {
                tracing::warn!(
                    "Failed to open the shared Redis pub/sub connection: {:?}",
                    err
                );
            }
        }

        // The subscriptions died with the socket. Dropping the broadcast senders
        // ends every listener's stream, which ends its SSE response and makes the
        // client reconnect into a fresh snapshot — rather than leaving it holding an
        // open connection that will never speak again.
        drop_all_listeners(&registry);

        if !wait_out_the_outage(&mut commands, delay).await {
            return;
        }
        delay = (delay * 2).min(RECONNECT_MAX_DELAY);
    }
}

/// Re-issues `SUBSCRIBE` for every channel the registry still holds.
async fn resubscribe_all(
    sink: &mut PubSubSink,
    registry: &Arc<Mutex<Registry>>,
) -> Result<(), redis::RedisError> {
    let channels: Vec<String> = {
        let registry = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        registry.channels.keys().cloned().collect()
    };

    for channel in channels {
        sink.subscribe(&channel).await?;
    }
    Ok(())
}

/// Runs one connection: dispatches inbound messages and applies queued commands
/// until either the connection dies or the process shuts down.
async fn serve(
    sink: &mut PubSubSink,
    mut stream: PubSubStream,
    commands: &mut mpsc::UnboundedReceiver<Command>,
    registry: &Arc<Mutex<Registry>>,
) -> ServeExit {
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(Command::Subscribe { channel, ack }) => {
                    let result = sink.subscribe(&channel).await;
                    let failed = result.is_err();
                    // The caller is blocked on this before it reads its snapshot, so
                    // answer before deciding what to do about the connection.
                    let _ = ack.send(result.map_err(|err| err.to_string()));
                    if failed {
                        return ServeExit::ConnectionLost;
                    }
                }
                Some(Command::Unsubscribe { channel }) => {
                    if let Err(err) = sink.unsubscribe(&channel).await {
                        tracing::warn!(
                            "Failed to unsubscribe from Redis channel {}: {:?}",
                            channel,
                            err
                        );
                        return ServeExit::ConnectionLost;
                    }
                }
                None => return ServeExit::ShuttingDown,
            },
            message = stream.next() => match message {
                Some(msg) => {
                    let channel = msg.get_channel_name().to_string();
                    match msg.get_payload::<String>() {
                        Ok(payload) => deliver_to_listeners(registry, &channel, payload),
                        Err(err) => tracing::warn!(
                            "Unreadable payload on Redis channel {}: {:?}",
                            channel,
                            err
                        ),
                    }
                }
                None => return ServeExit::ConnectionLost,
            },
        }
    }
}

/// Ends every live listener by dropping the broadcast senders the registry holds.
fn drop_all_listeners(registry: &Arc<Mutex<Registry>>) {
    let mut registry = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    registry.channels.clear();
}

/// Sits out the reconnect backoff without leaving callers hanging.
///
/// A stream that asks to subscribe while Redis is unreachable is told so — it gets
/// the same failure the per-connection subscribe used to give it — rather than
/// waiting on an acknowledgement that cannot come until the outage ends.
///
/// Returns `false` when every [`SyncFanout`] handle is gone.
async fn wait_out_the_outage(
    commands: &mut mpsc::UnboundedReceiver<Command>,
    delay: Duration,
) -> bool {
    let backoff = tokio::time::sleep(delay);
    tokio::pin!(backoff);

    loop {
        tokio::select! {
            _ = &mut backoff => return true,
            command = commands.recv() => match command {
                Some(Command::Subscribe { ack, .. }) => {
                    let _ = ack.send(Err(
                        "the shared Redis pub/sub connection is down".to_string()
                    ));
                }
                // The socket that held the subscription is gone, so there is nothing
                // to unsubscribe from; the registry entry has already been removed.
                Some(Command::Unsubscribe { .. }) => {}
                None => return false,
            },
        }
    }
}

/// A fan-out whose Redis connection is replaced by a stub the test drives.
///
/// The manager task is the only part that needs a live Redis, and CI has none, so
/// the registry, reference counting and ordering guarantees — the parts that are
/// easy to get wrong — are exercised through this seam rather than skipped.
#[cfg(test)]
pub mod testing {
    use super::*;

    /// A `SUBSCRIBE`/`UNSUBSCRIBE` the shared connection was asked to issue.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum RedisOp {
        Subscribe(String),
        Unsubscribe(String),
    }

    /// The commands a stubbed fan-out's manager received, in order.
    #[derive(Clone, Default)]
    pub struct OpLog(Arc<Mutex<Vec<RedisOp>>>);

    impl OpLog {
        pub fn ops(&self) -> Vec<RedisOp> {
            self.0.lock().unwrap().clone()
        }

        /// Waits for the log to reach `count` entries. Unsubscribes are sent from a
        /// `Drop`, so there is no completion to await other than this.
        pub async fn wait_for(&self, count: usize) -> Vec<RedisOp> {
            for _ in 0..200 {
                let ops = self.ops();
                if ops.len() >= count {
                    return ops;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!(
                "timed out waiting for {count} Redis ops, saw {:?}",
                self.ops()
            );
        }
    }

    impl SyncFanout {
        /// Hands one payload to every listener on `channel`, exactly as the manager
        /// task's message path does.
        pub fn deliver_for_test(&self, channel: &str, payload: &str) {
            deliver_to_listeners(&self.registry, channel, payload.to_string());
        }

        /// Simulates the shared connection dropping, exactly as the manager task
        /// does before it reconnects.
        pub fn drop_connection_for_test(&self) {
            drop_all_listeners(&self.registry);
        }
    }

    /// Builds a fan-out backed by a stub manager that acknowledges every subscribe
    /// and records what it was asked to do.
    pub fn stubbed_fanout() -> (Arc<SyncFanout>, OpLog) {
        let registry = Arc::new(Mutex::new(Registry::default()));
        let (commands, mut receiver) = mpsc::unbounded_channel();
        let log = OpLog::default();

        let recorded = log.clone();
        tokio::spawn(async move {
            while let Some(command) = receiver.recv().await {
                match command {
                    Command::Subscribe { channel, ack } => {
                        recorded.0.lock().unwrap().push(RedisOp::Subscribe(channel));
                        let _ = ack.send(Ok(()));
                    }
                    Command::Unsubscribe { channel } => {
                        recorded
                            .0
                            .lock()
                            .unwrap()
                            .push(RedisOp::Unsubscribe(channel));
                    }
                }
            }
        });

        (Arc::new(SyncFanout { registry, commands }), log)
    }
}
