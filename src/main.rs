pub mod routes;
pub mod state;
pub mod auth;
pub mod models;
pub mod dao;
pub mod jobs;
pub mod observability;

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
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};

/// Connects the pool without touching the schema. Split out from [`init_postgres`] so the
/// `reap-stale-users` job can reach the database without running migrations of its own.
async fn connect_postgres() -> Result<sqlx::Pool<sqlx::Postgres>, Box<dyn std::error::Error>> {
    let database_url = std::env::var("DATABASE_URL")?;

    // 2. Spin up the centralized thread connection pool
    Ok(PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await?)
}

async fn init_postgres() -> Result<sqlx::Pool<sqlx::Postgres>, Box<dyn std::error::Error>> {
    let pool = connect_postgres().await?;

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
    let google_client_ids = auth::client_ids::load_google_client_ids();
    if google_client_ids.is_empty() {
        // Not fatal: local dev signs in with `mock.` tokens, which skip the
        // audience check. In a real deployment this means every login fails.
        tracing::error!("No Google client IDs configured; set GOOGLE_IOS_CLIENT_IDS");
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

/// One sweep of the stale-account reaper, then exit. Deliberately does not build an
/// [`AppState`]: the job needs the database and Redis, and none of the auth or AI secrets.
async fn run_reaper() {
    let pool = connect_postgres()
        .await
        .expect("Failed to connect to PostgreSQL");
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://cache-svc:6379".to_string());
    let redis_client = redis::Client::open(redis_url).expect("Invalid Redis URL");

    let config = jobs::reap_stale_users::ReapConfig::from_env();
    match jobs::reap_stale_users::reap_stale_users(&pool, &redis_client, &config).await {
        Ok(_) => {}
        Err(err) => {
            tracing::error!("Stale account sweep failed: {:?}", err);
            std::process::exit(1);
        }
    }
}

#[tokio::main]
async fn main() {
    // Initialize structured JSON logging. `RUST_LOG` selects the level so a noisy
    // incident can be debugged with a rollout restart instead of a rebuild.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .init();

    // No subcommand serves the API; the CronJob passes `reap-stale-users`.
    match std::env::args().nth(1).as_deref() {
        None => serve().await,
        Some("reap-stale-users") => run_reaper().await,
        Some(other) => {
            eprintln!("unknown subcommand '{}'; expected 'reap-stale-users'", other);
            std::process::exit(2);
        }
    }
}

async fn serve() {
    // Installed before anything else so no startup metric is dropped on the floor.
    if let Some(handle) = observability::metrics::init_recorder() {
        tokio::spawn(observability::metrics::serve_metrics(handle));
    }

    let app_state = init_app_state().await;
    // Metadata only — `db_health` reads `size()` to tell an outage from load, and
    // never issues a query.
    observability::db_health::register_pool(app_state.db_pool.clone());

    // api routes group
    let api_routes = Router::new()
        .route("/sync", axum::routing::post(routes::sync::sync_handler))
        .route("/sync/status", axum::routing::get(routes::sync::status::sync_status_handler))
        .route("/sync/stream", axum::routing::get(routes::sync::stream::sync_stream_handler))
        .route("/v1/sync/stream", axum::routing::get(routes::sync::stream::sync_stream_handler))
        .route("/categorize", axum::routing::post(routes::ai::handlers::categorize_item_handler))

        .route("/assign-icon", axum::routing::post(routes::ai::handlers::assign_todo_icon_handler))
        .route("/devices", axum::routing::get(routes::devices::list_devices_handler))
        .route("/devices", axum::routing::post(routes::devices::register_device_handler))
        .route("/devices/:id", axum::routing::patch(routes::devices::rename_device_handler))
        .route("/user/data", axum::routing::delete(routes::user::delete_user_data_handler))
        .route("/lists/invite", axum::routing::post(routes::lists::invite_handler))
        .route("/lists/join", axum::routing::post(routes::lists::join_handler))
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
        // Superseded by `/healthz/live`. Kept until the cluster's probes have
        // been repointed, so this deploy cannot strand a rollout; delete after.
        .route("/healthcheck", get(|| async { "OK" }))
        .merge(observability::health::health_routes(app_state.redis_client.clone()))
        .nest("/api", api_routes)
        .nest("/auth", auth_routes)
        .layer(cors)
        // Read bottom-up: `Router::layer` makes the *last* call the outermost, so
        // this runs SetRequestId → track_request → Propagate. The order matters and
        // is not cosmetic — SetRequestId must precede the middleware that logs the
        // id, and Propagate must sit inside both so it sees the stamped request on
        // the way in and can copy the id onto the response on the way out.
        // `track_request` is outside `require_auth`, so rejected requests are
        // measured too.
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(middleware::from_fn(observability::http::track_request))
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid));

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
