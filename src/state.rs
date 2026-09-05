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

