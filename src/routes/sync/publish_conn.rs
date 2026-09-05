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
//!
//! The same argument applies to every other bit of Redis traffic on the tail of a
//! sync, not just the publishes it was written for, so the wrapper also hands out
//! [`RedisPublisher::run_pipeline`] and [`RedisPublisher::query`]: the sync-status
//! cache writes go through this one connection too, instead of dialling their own.

use std::future::Future;
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
        let payload = &payload;
        self.with_redial("publish", |mut conn| async move {
            conn.publish::<_, _, ()>(channel, payload).await
        })
        .await
    }

    /// Runs a whole pipeline over the cached connection: one round trip for the batch.
    ///
    /// The sync tail used to walk its work item by item — a `PUBLISH` per config
    /// broadcast, then two `SET`s per user whose caches the write invalidated — and
    /// every one of those was a serial round trip held open inside the request, after
    /// the transaction had already committed. Batching them costs nothing in
    /// correctness (none of these commands reads a value another one writes) and turns
    /// a count that grows with the payload into a constant.
    ///
    /// All-or-nothing is the tradeoff: where per-command sends could fail one publish
    /// and deliver the rest, a dead socket now loses the batch. That matches what these
    /// callers already do about a failure — log it and carry on — and the retry below
    /// covers the case that actually happens in production, which is the whole
    /// connection having gone away rather than one command being rejected.
    pub async fn run_pipeline(&self, pipe: &redis::Pipeline) -> Result<(), AppError> {
        self.with_redial("pipeline", |mut conn| async move {
            pipe.query_async::<()>(&mut conn).await
        })
        .await
    }

    /// Runs one command over the cached connection and decodes its reply.
    ///
    /// For the read side of the sync-status cache, which wants the value back rather
    /// than fire-and-forget.
    pub async fn query<T: redis::FromRedisValue>(&self, cmd: &redis::Cmd) -> Result<T, AppError> {
        self.with_redial("command", |mut conn| async move {
            cmd.query_async::<T>(&mut conn).await
        })
        .await
    }

    /// Runs one Redis interaction, redialling once if the cached connection has died.
    ///
    /// Takes a closure rather than a command because the callers want different shapes
    /// — a single `PUBLISH`, a `GET` decoded into an `Option<String>`, a pipeline of
    /// `SET`s — and the part worth sharing between them is the redial, not the command.
    ///
    /// `op` therefore has to be replayable: it runs a second time after a failure. Every
    /// caller here sends commands that are safe to repeat — an overwrite keyed by name,
    /// or a fan-out event listeners already treat idempotently — which is the same
    /// bargain [`Self::publish`] has always made, and the retry stays bounded to one
    /// attempt for the same reason it always was: a best-effort write must not turn into
    /// a loop holding a request open.
    async fn with_redial<T, F, Fut>(&self, what: &str, op: F) -> Result<T, AppError>
    where
        F: Fn(MultiplexedConnection) -> Fut,
        Fut: Future<Output = redis::RedisResult<T>>,
    {
        let (generation, conn) = self.connection().await?;

        match op(conn).await {
            Ok(value) => Ok(value),
            Err(err) => {
                tracing::warn!(
                    "Redis {} failed on the cached connection, redialling: {:?}",
                    what,
                    err
                );
                self.discard(generation).await;
                let (_, fresh) = self.connection().await?;
                Ok(op(fresh).await?)
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
