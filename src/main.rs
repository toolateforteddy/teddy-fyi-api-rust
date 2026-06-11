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
    match sqlx::query("SELECT 1").execute(&state.db_pool).await {
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

#[tokio::main]
async fn main() {
    // Initialize structured JSON logging
    tracing_subscriber::fmt().json().init();

    let client_id = std::env::var("GOOGLE_CLIENT_ID").unwrap_or_default();
    let jwt_secret = std::env::var("JWT_SECRET").expect("JWT_SECRET must be set");
    let app_state = AppState {
        client_id: client_id.clone(),
        google_client: Arc::new(google_oauth::AsyncClient::new(&client_id)),
        db_pool: init_postgres()
            .await
            .expect("Failed to initialize PostgreSQL"),
        jwt_secret,
    };

    // api routes group
    let api_routes = Router::new()
        .route("/sync", axum::routing::post(routes::sync::sync_handler))
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
        .with_state(app_state.clone());

    // Build our application with multiple routes
    let app = Router::new()
        .route("/hello", get(|| async { "world" }))
        .route("/hellov2", get(|| async { "world2" }))
        .route("/healthcheck", get(|| async { "OK" })) // Shallow/Liveness check
        .nest("/api", api_routes)
        .nest("/auth", auth_routes);

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
