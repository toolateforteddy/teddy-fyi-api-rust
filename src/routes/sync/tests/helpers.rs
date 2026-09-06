use crate::state::AppState;
use crate::routes::sync::{SyncRequest, SyncResponse, AppJson, sync_handler as parent_sync_handler, AppError};
use crate::auth::tokens::Claims;
use axum::{extract::State, Extension, Json};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

/// The one place a `SyncRequest` is written out in full.
///
/// Every field of the request is `#[serde(default)]` except `client_id` and
/// `last_synced_at`, so a client sending nothing but a client id is a real request and
/// this is what the server actually receives from it. Tests build on that with struct
/// update syntax, naming only the part they are about:
///
/// ```ignore
/// let req = SyncRequest { scope: Some(SyncScope::Todo), ..request("client-1") };
/// ```
///
/// The point is that a new field on `SyncRequest` lands here and nowhere else. Two
/// separate branches have now merged cleanly as text and left `main` unable to compile,
/// because one added a field while the other added a literal of the struct in a file the
/// first never touched; with the literals routed through here there is one site to update
/// and nothing for a second branch to collide with.
pub fn request(client_id: &str) -> SyncRequest {
    SyncRequest {
        last_synced_at: None,
        client_id: client_id.to_string(),
        device_uuid: None,
        device_name: None,
        scope: None,
        todo_list_changes: vec![],
        todo_changes: vec![],
        grocery_list_changes: vec![],
        grocery_list_member_changes: vec![],
        store_changes: vec![],
        category_changes: vec![],
        grocery_changes: vec![],
        grocery_item_store_info_changes: vec![],
        config_changes: vec![],
        drawing_changes: vec![],
        configs: vec![],
        drawings: vec![],
        supports_paging: false,
    }
}

pub fn setup_state(pool: PgPool) -> AppState {
    let redis_client = redis::Client::open(
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string()),
    )
    .unwrap();

    AppState {
        // Classified the way a fully configured deployment is, so a test that reaches for
        // an audience gets a product with it. See `crate::auth::client_ids`.
        client_catalog: Arc::new(crate::auth::client_ids::ClientCatalog::build(
            [
                ("test-client".to_string(), crate::auth::product::Product::TeddyFyi),
                ("test-web-client".to_string(), crate::auth::product::Product::TeddyFyi),
                (
                    "test-scribbleroute-client".to_string(),
                    crate::auth::product::Product::ScribbleRoute,
                ),
            ],
            Vec::new(),
        )),
        google_client: Arc::new(google_oauth::AsyncClient::new("test-client")),
        db_pool: pool,
        jwt_secret: "test-secret".to_string(),
        gemini_api_key: Some("test-key".to_string()),
        http_client: crate::routes::ai::gemini::build_http_client(),
        sync_fanout: crate::routes::sync::fanout::SyncFanout::spawn(redis_client.clone()),
        redis_publisher: Arc::new(crate::routes::sync::publish_conn::RedisPublisher::new(
            redis_client.clone(),
        )),
        redis_client,
        cookie_domain: ".teddy.fyi".to_string(),
        stream_slots: Arc::new(crate::routes::sync::stream_limits::StreamSlots::from_env()),
    }
}

/// Calls the real handler as a *legitimate* client: the token names the same device the body
/// does.
///
/// The claim used to be hardcoded to `client-1` regardless of what the request said, which was
/// harmless while the body field was the only thing the handler read. It is not harmless now —
/// the handler binds the device to the token, so a hardcoded claim would make every test here
/// silently exercise the mismatch path instead of the behaviour it was written for. Tests that
/// want the two to disagree say so explicitly by calling the handler themselves; see
/// `super::client_binding`.
pub async fn sync_handler(
    state: State<AppState>,
    req: AppJson<SyncRequest>,
) -> Result<Json<SyncResponse>, AppError> {
    let claims = Claims {
        sub: "user-1".to_string(),
        client_uuid: req.0.client_id.clone(),
        exp: 10000000000,
        product: None,
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
