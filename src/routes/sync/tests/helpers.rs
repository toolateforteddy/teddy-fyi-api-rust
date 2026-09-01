use crate::state::AppState;
use crate::routes::sync::{SyncRequest, SyncResponse, AppJson, sync_handler as parent_sync_handler, AppError};
use crate::auth::tokens::Claims;
use axum::{extract::State, Extension, Json};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

pub fn setup_state(pool: PgPool) -> AppState {
    AppState {
        google_client_ids: [
            "test-client".to_string(),
            "test-web-client".to_string(),
            "test-scribbleroute-client".to_string(),
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        google_client: Arc::new(google_oauth::AsyncClient::new("test-client")),
        db_pool: pool,
        jwt_secret: "test-secret".to_string(),
        gemini_api_key: "test-key".to_string(),
        redis_client: redis::Client::open(
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
        )
        .unwrap(),
        cookie_domain: ".teddy.fyi".to_string(),
    }
}

pub async fn sync_handler(
    state: State<AppState>,
    req: AppJson<SyncRequest>,
) -> Result<Json<SyncResponse>, AppError> {
    let claims = Claims {
        sub: "user-1".to_string(),
        client_uuid: "client-1".to_string(),
        exp: 10000000000,
    };
    parent_sync_handler(state, Extension(claims), req).await
}

/// Seeds a device for `user_uuid` and returns its id. Standing in for the row the
/// migration backfills, this is the device a request without a `device_uuid` resolves to.
pub async fn seed_device(pool: &PgPool, user_uuid: Uuid, name: &str) -> Uuid {
    let device_uuid = Uuid::new_v4();
    sqlx::query!(
        "INSERT INTO devices (id, user_id, name) VALUES ($1, $2, $3)",
        device_uuid,
        user_uuid,
        name
    )
    .execute(pool)
    .await
    .unwrap();
    device_uuid
}
