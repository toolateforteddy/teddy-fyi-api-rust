use axum::{
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::get,
    Router,
};
use std::sync::Arc;

#[derive(Clone)]
struct AppState {
    google_client: Arc<google_oauth::AsyncClient>,
}

#[tokio::main]
async fn main() {
    // Initialize structured JSON logging
    tracing_subscriber::fmt().json().init();

    let client_id = std::env::var("GOOGLE_CLIENT_ID").unwrap_or_default();
    let app_state = AppState {
        google_client: Arc::new(google_oauth::AsyncClient::new(&client_id)),
    };

    // Authed routes group
    let authed_routes = Router::new()
        .route("/hello", get(|| async { "authed/world" }))
        .route_layer(middleware::from_fn_with_state(app_state, require_google_auth));

    // Build our application with multiple routes
    let app = Router::new()
        .route("/hello", get(|| async { "world" }))
        .route("/hellov2", get(|| async { "world2" }))
        .route("/healthcheck", get(|| async { "OK" }))
        .nest("/authed", authed_routes);

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

/// Middleware to check for a valid Google Auth token
async fn require_google_auth(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let auth_header = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .filter(|s| s.starts_with("Bearer "));

    let _token = match auth_header {
        Some(header) => &header["Bearer ".len()..],
        None => return Err(StatusCode::UNAUTHORIZED),
    };

    match state.google_client.validate_id_token(_token).await {
        Ok(_verified_token) => {}
        Err(err) => {
            tracing::warn!("Token verification failed: {:?}", err);
            return Err(StatusCode::UNAUTHORIZED);
        }
    }

    Ok(next.run(req).await)
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    tracing::info!("Shutting down gracefully...");
}
