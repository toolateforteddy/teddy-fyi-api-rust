pub mod routes;
pub mod state;
pub mod auth;

use axum::{
    extract::State,
    http::StatusCode,
    middleware,
    response::IntoResponse,
    routing::get,
    Router,
};
use sqlx::postgres::PgPoolOptions;
use state::AppState;
use std::sync::Arc;

async fn init_postgres() -> Result<sqlx::Pool<sqlx::Postgres>, Box<dyn std::error::Error>> {
    let database_url = std::env::var("DATABASE_URL")?;

    // 2. Spin up the centralized thread connection pool
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?;

    // 3. FORCE RUN OUTSTANDING MIGRATIONS ON STARTUP
    // This looks at our local `/migrations` folder and updates Neon instantly
    sqlx::migrate!("./migrations").run(&pool).await?;

    println!("🚀 Database successfully synced and serverless migrations verified!");
    Ok(pool)
}

async fn readiness_handler(State(state): State<AppState>) -> impl IntoResponse {
    // Ping the database
    match sqlx::query!("SELECT 1 as one").fetch_one(&state.db_pool).await {
        Ok(_) => (StatusCode::OK, "OK").into_response(),
        Err(err) => {
            tracing::error!("Readiness probe database connection failed: {:?}", err);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database connection unhealthy",
            )
                .into_response()
        }
    }
}

async fn init_app_state() -> AppState {
    let client_id = std::env::var("GOOGLE_CLIENT_ID").unwrap_or_default();
    let web_client_id = std::env::var("GOOGLE_CLIENT_ID_GROCERY_WEB").unwrap_or_default();
    let scribbleroute_client_id = std::env::var("SCRIBBLEROUTE_API_CLIENT_ID").unwrap_or_default();

    let mut google_client_ids = std::collections::HashSet::new();
    if !client_id.is_empty() {
        google_client_ids.insert(client_id);
    }
    if !web_client_id.is_empty() {
        google_client_ids.insert(web_client_id);
    }
    if !scribbleroute_client_id.is_empty() {
        google_client_ids.insert(scribbleroute_client_id);
    }

    let jwt_secret = std::env::var("JWT_SECRET").expect("JWT_SECRET must be set");
    let gemini_api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://cache-svc:6379".to_string());
    let redis_client = redis::Client::open(redis_url).expect("Invalid Redis URL");
    let cookie_domain = std::env::var("COOKIE_DOMAIN").unwrap_or_else(|_| ".teddy.fyi".to_string());
    
    AppState {
        google_client_ids,
        google_client: Arc::new(google_oauth::AsyncClient::new("")),
        db_pool: init_postgres()
            .await
            .expect("Failed to initialize PostgreSQL"),
        jwt_secret,
        gemini_api_key,
        redis_client,
        cookie_domain,
    }
}

#[tokio::main]
async fn main() {
    // Initialize structured JSON logging
    tracing_subscriber::fmt().json().init();

    let app_state = init_app_state().await;

    // api routes group
    let api_routes = Router::new()
        .route("/sync", axum::routing::post(routes::sync::sync_handler))
        .route("/sync/status", axum::routing::get(routes::sync::status::sync_status_handler))
        .route("/categorize", axum::routing::post(routes::ai::handlers::categorize_item_handler))
        .route("/assign-icon", axum::routing::post(routes::ai::handlers::assign_todo_icon_handler))
        .route("/hc", get(|| async { "OK" }))
        .route("/ready", get(readiness_handler)) // Deep/Readiness check
        .route_layer(middleware::from_fn_with_state(
            app_state.clone(),
            auth::middleware::require_auth,
        ))
        .with_state(app_state.clone());

    // Public auth routes
    let auth_routes = Router::new()
        .route("/login", axum::routing::post(auth::handlers::login_handler))
        .route("/refresh", axum::routing::post(auth::handlers::refresh_handler))
        .route("/logout", axum::routing::post(auth::handlers::logout_handler))
        .with_state(app_state.clone());

    // Explicit CORS Configurations:
    // - allow_origin: Must explicitly point to https://teddy.fyi.
    // - allow_credentials: Set to true.
    // - allow_methods: Explicitly allow GET, POST, PUT, DELETE, OPTIONS.
    // - allow_headers: Explicitly allow Content-Type, Authorization, and X-Client-UUID.
    let cors = tower_http::cors::CorsLayer::new()
        .allow_origin("https://teddy.fyi".parse::<axum::http::HeaderValue>().unwrap())
        .allow_credentials(true)
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::DELETE,
            axum::http::Method::OPTIONS,
        ])
        .allow_headers([
            axum::http::header::CONTENT_TYPE,
            axum::http::header::AUTHORIZATION,
            axum::http::HeaderName::from_static("x-client-uuid"),
        ]);

    // Build our application with multiple routes
    let app = Router::new()
        .route("/hello", get(|| async { "world" }))
        .route("/hellov2", get(|| async { "world2" }))
        .route("/healthcheck", get(|| async { "OK" })) // Shallow/Liveness check
        .nest("/api", api_routes)
        .nest("/auth", auth_routes)
        .layer(cors);

    // Read the port from the environment, falling back to 3000
    let port = std::env::var("PORT").unwrap_or_else(|_| "3000".to_string());
    let addr = format!("0.0.0.0:{}", port);

    // Bind to 0.0.0.0 so it is accessible outside the Docker container
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    tracing::info!("Listening on {}", listener.local_addr().unwrap());

    // Start serving the Axum application
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("Shutting down gracefully...");
}
