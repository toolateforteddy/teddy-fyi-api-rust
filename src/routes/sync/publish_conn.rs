//! The other half of "one Redis connection per process": the connection events are
//! published *on*.
//!
//! [`super::publisher`] used to call `get_multiplexed_async_connection()` on every
//! publish, so ordinary write traffic dialled — and threw away — a Redis connection
//! per event. That is a TCP handshake (and, in production, a TLS one) on the tail of
//! every sync request, and under load it churns exactly the `maxclients` budget the
//! SSE cap exists to protect.
//!
//! A multiplexed connection is the right thing to hold onto: it is `Clone`, it
//! pipelines concurrent commands over one socket, and it is safe to share across
//! tasks. What it does **not** do is heal itself — once its socket dies every later
//! command on it fails — so this wrapper adds the one piece it is missing: notice
//! the failure, throw the dead connection away, and dial once more.

use std::sync::atomic::{AtomicU64, Ordering};

use redis::aio::MultiplexedConnection;
use redis::AsyncCommands;
use tokio::sync::Mutex;

use super::types::AppError;

/// Identifies which dial produced a cached connection, so a publisher that fails on
/// a stale clone cannot evict the *replacement* another task has already installed.
type Generation = u64;

/// A lazily dialled, shared Redis connection for publishing sync events.
pub struct RedisPublisher {
    client: redis::Client,
    /// A `tokio::Mutex` rather than a `std` one because the dial happens while it is
    /// held — on purpose: concurrent publishers arriving during a dial queue behind
    /// it and get the same connection, instead of opening one apiece and defeating
    /// the point of caching.
    cached: Mutex<Option<(Generation, MultiplexedConnection)>>,
    generations: AtomicU64,
}

impl RedisPublisher {
    /// Wraps a client. Cheap and non-async: nothing is dialled until the first
    /// publish, so Redis being down at startup does not stop the process booting.
    pub fn new(client: redis::Client) -> Self {
        Self {
            client,
            cached: Mutex::new(None),
            generations: AtomicU64::new(0),
        }
    }

    /// The underlying client, for the paths that still need one of their own (the
    /// cache invalidation in account deletion, and `/healthz/ready`).
    pub fn client(&self) -> &redis::Client {
        &self.client
    }

    /// Publishes one payload, redialling once if the cached connection has died.
    ///
    /// The retry is deliberately bounded to a single attempt. A publish is
    /// best-effort fan-out — the durable copy is already committed in Postgres and
    /// clients reconcile by version on their next sync — so it must not turn into an
    /// unbounded retry loop holding a request open. A retry can in principle deliver
    /// the same event twice (if the failure came after Redis accepted it), which is
    /// harmless: every event a listener acts on is idempotent, an overwrite by key
    /// or an invalidation.
    pub async fn publish(&self, channel: &str, payload: String) -> Result<(), AppError> {
        let (generation, mut conn) = self.connection().await?;

        match conn.publish::<_, _, ()>(channel, &payload).await {
            Ok(()) => Ok(()),
            Err(err) => {
                tracing::warn!(
                    "Redis publish failed on the cached connection, redialling: {:?}",
                    err
                );
                self.discard(generation).await;
                let (_, mut fresh) = self.connection().await?;
                fresh.publish::<_, _, ()>(channel, &payload).await?;
                Ok(())
            }
        }
    }

    /// Hands out a clone of the cached connection, dialling one if there is none.
    async fn connection(&self) -> Result<(Generation, MultiplexedConnection), AppError> {
        let mut cached = self.cached.lock().await;

        if let Some((generation, conn)) = cached.as_ref() {
            return Ok((*generation, conn.clone()));
        }

        let conn = self.client.get_multiplexed_async_connection().await?;
        let generation = self.generations.fetch_add(1, Ordering::Relaxed);
        *cached = Some((generation, conn.clone()));
        Ok((generation, conn))
    }

    /// Evicts the cached connection, but only if it is still the one that failed.
    async fn discard(&self, generation: Generation) {
        let mut cached = self.cached.lock().await;
        if cached.as_ref().is_some_and(|(current, _)| *current == generation) {
            *cached = None;
        }
    }
}
