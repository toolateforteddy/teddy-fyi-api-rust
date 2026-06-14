use std::sync::Arc;

#[derive(Clone)]
pub struct AppState {
    pub client_id: String,
    pub web_client_id: String,
    pub google_client: Arc<google_oauth::AsyncClient>,
    pub db_pool: sqlx::Pool<sqlx::Postgres>,
    pub jwt_secret: String,
    pub gemini_api_key: String,
}
