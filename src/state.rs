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
}

