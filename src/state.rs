use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    /// Every Google client ID accepted as a token audience, and the product each belongs
    /// to. Behind an `Arc` because `AppState` is cloned per request and this is a map that
    /// is built once at start-up and only ever read. See [`crate::auth::client_ids`].
    pub client_catalog: Arc<crate::auth::client_ids::ClientCatalog>,
    pub google_client: Arc<google_oauth::AsyncClient>,
    pub db_pool: sqlx::Pool<sqlx::Postgres>,
    pub jwt_secret: String,
    /// The Gemini API key, or `None` on a deployment that does not set one.
    ///
    /// `Option`, not `String`, because the AI endpoints are teddy.fyi's alone and this
    /// service is about to be forked. `init_app_state` used to `expect` this variable, so a
    /// ScribbleRoute deployment that dropped it would crash-loop on boot — the split plan's
    /// risk register carries that as an entry, and the remedy it prescribes is a
    /// three-way simultaneous edit of the code, this field and the manifest. Making the
    /// key optional turns that into a no-op: drop the variable and the AI endpoints answer
    /// 503, which is what a deployment without them should say anyway.
    ///
    /// Read through [`crate::routes::ai::require_gemini_api_key`], never directly, so that
    /// "absent" has exactly one meaning at every call site.
    pub gemini_api_key: Option<String>,
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

