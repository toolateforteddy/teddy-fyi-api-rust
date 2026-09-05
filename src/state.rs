use std::collections::HashSet;
use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub google_client_ids: HashSet<String>,
    pub google_client: Arc<google_oauth::AsyncClient>,
    pub db_pool: sqlx::Pool<sqlx::Postgres>,
    pub jwt_secret: String,
    pub gemini_api_key: String,
    pub redis_client: redis::Client,
    /// One HTTP client for outbound Gemini calls, shared by every request.
    ///
    /// `reqwest::Client` is itself an `Arc` around a connection pool, so cloning
    /// `AppState` per request is cheap and every clone reuses the same pool and
    /// TLS setup. Held here rather than built per call because the per-call
    /// version paid a fresh handshake every time and bounded nothing; see
    /// [`crate::routes::ai::gemini::build_http_client`], which also sets the
    /// outbound timeout.
    pub http_client: reqwest::Client,
    /// The process-wide Redis Pub/Sub subscriber every SSE stream fans out from.
    ///
    /// One connection for the whole replica rather than one per open stream: see
    /// [`crate::routes::sync::fanout`] for why the per-stream version was a
    /// service-wide hazard and how the ordering the handler depends on survives
    /// the change.
    pub sync_fanout: Arc<crate::routes::sync::fanout::SyncFanout>,
    /// The cached connection sync events are published *on*, so a write no longer
    /// dials Redis per event. See [`crate::routes::sync::publish_conn`].
    pub redis_publisher: Arc<crate::routes::sync::publish_conn::RedisPublisher>,
    /// `Domain` attribute for the `access_token` cookie, from `COOKIE_DOMAIN`. Empty means
    /// no `Domain` attribute at all — a host-only cookie, and a perfectly ordinary
    /// single-host deployment.
    ///
    /// Read only by [`crate::auth::handlers::session_cookie`], and it must stay that way.
    /// It used to double as the gate on the `mock.` login bypass, which made "empty" a
    /// silent switch for impersonating any account; see [`crate::auth::dev_bypass`].
    pub cookie_domain: String,
    /// Live `/api/sync/stream` counts, per account and per process.
    ///
    /// Shared rather than per-request state because a cap is only meaningful
    /// across connections: `AppState` is cloned per request, so the counters
    /// themselves sit behind an `Arc` and every clone bumps the same map. See
    /// [`crate::routes::sync::stream_limits`] for why the endpoint needs a cap
    /// at all and why the primitive inside is a plain `std::sync::Mutex`.
    pub stream_slots: Arc<crate::routes::sync::stream_limits::StreamSlots>,
}

