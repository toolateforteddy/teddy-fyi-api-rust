pub mod guardrails;
pub mod routes;
pub mod state;
pub mod db;
pub mod auth;
pub mod models;
pub mod dao;
pub mod jobs;
pub mod observability;
pub mod rate_limit;

use axum::{
    extract::State,
    http::StatusCode,
    middleware,
    response::IntoResponse,
    routing::get,
    Router,
};
use state::AppState;
use std::sync::Arc;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};

/// Browser origins allowed to call this API, from `CORS_ALLOWED_ORIGINS` (comma-separated).
///
/// The default carries teddy.fyi, which is all this service allowed before device pairing,
/// plus both spellings of the ScribbleRoute site: `/auth/device/claim` is called from the
/// `/link` page in a parent's browser, and a blocked preflight there is the difference
/// between a Fire tablet being able to sign in and not.
///
/// An unparseable entry is dropped with a warning rather than panicking the process: a typo
/// in a manifest should cost one origin, not the whole service.
fn allowed_origins() -> Vec<axum::http::HeaderValue> {
    const DEFAULT_ORIGINS: &str =
        "https://teddy.fyi,https://scribbleroute.com,https://www.scribbleroute.com";

    let raw = std::env::var("CORS_ALLOWED_ORIGINS")
        .ok()
        .filter(|raw| !raw.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_ORIGINS.to_string());

    let origins: Vec<axum::http::HeaderValue> = raw
        .split(',')
        .map(str::trim)
        .filter(|origin| !origin.is_empty())
        .filter_map(|origin| match origin.parse::<axum::http::HeaderValue>() {
            Ok(value) => Some(value),
            Err(err) => {
                tracing::error!("Ignoring unparseable CORS origin {:?}: {:?}", origin, err);
                None
            }
        })
        .collect();

    if origins.is_empty() {
        tracing::error!("No usable CORS origins configured; browser clients will be blocked");
    }
    origins
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
    let client_catalog = auth::client_ids::load_client_catalog();
    // Fatal, in both directions and for opposite reasons: a shipped binary with no
    // audience allowlist can never authenticate anybody, and a `dev-auth` binary that has
    // one is a development build wearing production credentials. Both used to be survivable
    // — the first was one `tracing::error!` line and then every login failing for the life
    // of the process. Crashing here is confined to start-up, so it fails a rollout rather
    // than a request. The full argument is on `assert_startup_config`.
    auth::dev_bypass::assert_startup_config(&client_catalog);

    // The remaining classification work, stated at boot rather than left to be discovered
    // by reading `client_ids.rs`. An unclassified audience still signs in; what it cannot
    // do is carry a product claim, so its sessions are the ones the scope check has to wave
    // through. Moving an ID out of this line is a configuration change and no deploy --
    // list it under TEDDY_FYI_CLIENT_IDS or SCRIBBLEROUTE_CLIENT_IDS.
    let unclassified = client_catalog.unclassified();
    if unclassified.is_empty() {
        tracing::info!(
            teddy_fyi_client_ids = client_catalog.classified_count(auth::product::Product::TeddyFyi),
            scribbleroute_client_ids =
                client_catalog.classified_count(auth::product::Product::ScribbleRoute),
            "Every configured Google client ID is classified per product"
        );
    } else {
        tracing::warn!(
            teddy_fyi_client_ids = client_catalog.classified_count(auth::product::Product::TeddyFyi),
            scribbleroute_client_ids =
                client_catalog.classified_count(auth::product::Product::ScribbleRoute),
            unclassified = ?unclassified,
            "Google client IDs with no product: sessions established through these carry no \
             product claim and their sync scopes cannot be enforced"
        );
    }

    let jwt_secret = std::env::var("JWT_SECRET").expect("JWT_SECRET must be set");
    let gemini_api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set");
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://cache-svc:6379".to_string());
    let redis_client = redis::Client::open(redis_url).expect("Invalid Redis URL");
    let cookie_domain = std::env::var("COOKIE_DOMAIN").unwrap_or_else(|_| ".teddy.fyi".to_string());
    
    AppState {
        client_catalog: Arc::new(client_catalog),
        google_client: Arc::new(google_oauth::AsyncClient::new("")),
        db_pool: db::init_postgres()
            .await
            .expect("Failed to initialize PostgreSQL"),
        jwt_secret,
        gemini_api_key,
        // One shared pub/sub connection and one cached publish connection for the
        // whole process, both built from the same client. Neither dials Redis here:
        // a replica that cannot reach Redis at boot should still start and recover.
        sync_fanout: routes::sync::fanout::SyncFanout::spawn(redis_client.clone()),
        redis_publisher: Arc::new(routes::sync::publish_conn::RedisPublisher::new(
            redis_client.clone(),
        )),
        redis_client,
        http_client: routes::ai::gemini::build_http_client(),
        cookie_domain,
        // Read once at startup: the caps are deployment configuration, and a
        // per-request `env::var` on a hot path buys nothing.
        stream_slots: Arc::new(routes::sync::stream_limits::StreamSlots::from_env()),
    }
}

/// One sweep of the stale-account reaper, then exit. Deliberately does not build an
/// [`AppState`]: the job needs the database and Redis, and none of the auth or AI secrets.
///
/// Takes [`db::PoolConfig::reaper`] rather than the server's pool shape — a sequential
/// batch sweep wants two connections and the patience to sit out a Neon wake-up, which
/// is the opposite of what a request path wants. See [`crate::db`].
async fn run_reaper() {
    let pool = db::connect_postgres(&db::PoolConfig::reaper())
        .await
        .expect("Failed to connect to PostgreSQL");
    let redis_url =
        std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://cache-svc:6379".to_string());
    let redis_client = redis::Client::open(redis_url).expect("Invalid Redis URL");

    // Piggy-backs on the existing CronJob rather than adding a second schedule to the
    // cluster. Unlike the account sweep this is not gated on `REAP_DRY_RUN`: it deletes
    // only codes that are already dead, and it runs first so a failure in the (much
    // larger) account sweep cannot leave them piling up.
    match jobs::reap_device_authorizations::reap_device_authorizations(&pool).await {
        Ok(summary) => {
            tracing::info!(summary = ?summary, "Expired device authorizations swept");
        }
        Err(err) => {
            tracing::error!("Device authorization sweep failed: {:?}", err);
        }
    }

    // Same argument as the sweep above, for the same kind of rows: expired invites and
    // spent failure counters are already dead, so this is not gated on `REAP_DRY_RUN`
    // either, and a failure here must not stop the account sweep from running.
    match jobs::reap_list_invites::reap_list_invites(&pool).await {
        Ok(summary) => {
            tracing::info!(summary = ?summary, "Expired list invites swept");
        }
        Err(err) => {
            tracing::error!("List invite sweep failed: {:?}", err);
        }
    }

    let config = jobs::reap_stale_users::ReapConfig::from_env();
    // The sweep publishes an invalidation per deleted account; one cached connection
    // for the whole run rather than one dial per account.
    let publisher = routes::sync::publish_conn::RedisPublisher::new(redis_client);
    match jobs::reap_stale_users::reap_stale_users(&pool, &publisher, &config).await {
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

    // Bounds on time, body size and concurrency. Read once, here, so a misconfigured
    // value is logged at startup rather than discovered under load.
    let guardrails = guardrails::Guardrails::from_env();

    // The two Server-Sent Events endpoints, deliberately in a router of their own.
    //
    // These connections are *supposed* to stay open for the whole time the app is in
    // the foreground — they carry the real-time half of sync, and hold themselves open
    // with a 240-second keep-alive ping. A request deadline applied over the top of
    // them would sever every client's stream on a fixed timer and turn real-time sync
    // into 30-second polling, which is a far worse outage than the one the deadline is
    // there to prevent, and a silent one.
    //
    // Splitting them out is what makes that structural. The timeout below applies to
    // routers, not routes, so the only way an SSE endpoint can acquire a deadline is if
    // someone moves it back into `api_routes` — which is a visible edit, not an
    // accident. (Today's `TimeoutLayer` happens to bound the response *future* rather
    // than the response *body*, so it would not in fact cut a stream that has already
    // begun; that is an implementation detail of one layer, and not something the
    // routing should quietly depend on.)
    let api_stream_routes = Router::new()
        .route("/sync/stream", axum::routing::get(routes::sync::stream::sync_stream_handler))
        .route("/v1/sync/stream", axum::routing::get(routes::sync::stream::sync_stream_handler))
        .route_layer(middleware::from_fn_with_state(
            app_state.clone(),
            auth::middleware::require_auth,
        ))
        .with_state(app_state.clone());

    // api routes group
    let api_routes = Router::new()
        .route("/sync", axum::routing::post(routes::sync::sync_handler))
        .route("/sync/status", axum::routing::get(routes::sync::status::sync_status_handler))
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
        .with_state(app_state.clone())
        // Outside `require_auth` on purpose: the auth middleware talks to Postgres, so
        // it is itself something that can stall, and a deadline that starts after it
        // would not bound the request at all. Then the SSE endpoints are merged back in
        // underneath `/api`, having missed the layer.
        .layer(guardrails.timeout_layer())
        .merge(api_stream_routes);

    // Per-IP rate limits for the auth group. Built here rather than inside the router so the
    // limiter state can also be handed to the sweeper that drops buckets for addresses that
    // have gone quiet; see `rate_limit::auth_limits`.
    let auth_limit = rate_limit::auth_limits::Quota::general_auth().config();
    let device_start_limit = rate_limit::auth_limits::Quota::device_start().config();
    rate_limit::auth_limits::spawn_key_gc(auth_limit.clone());
    rate_limit::auth_limits::spawn_key_gc(device_start_limit.clone());

    // Public auth routes
    let auth_routes = Router::new()
        .route("/login", axum::routing::post(auth::handlers::login_handler))
        .route("/refresh", axum::routing::post(auth::handlers::refresh_handler))
        .route("/logout", axum::routing::post(auth::handlers::logout_handler))
        // Device pairing, for tablets with no Google identity of their own. All three are
        // unauthenticated by design: the tablet has no session yet, and the browser that
        // claims presents a Google ID token in the body rather than one of ours.
        //
        // `/device/start` carries a second, much tighter bucket of its own: it is the only
        // route here that runs an Argon2id hash (~19 MiB, tens of ms) before it knows
        // anything about the caller, so it is the one that turns a flood into an outage.
        .route(
            "/device/start",
            axum::routing::post(auth::device::start_handler)
                .layer(rate_limit::auth_limits::layer(device_start_limit)),
        )
        .route("/device/claim", axum::routing::post(auth::device::claim_handler))
        .route("/device/poll", axum::routing::post(auth::device::poll_handler))
        // Every one of these calls out to Google or Postgres before it answers, so they
        // get the same deadline as `/api`. None of them is long-lived: `/device/poll` is
        // a short poll that returns immediately, not a hanging GET.
        //
        // `route_layer` here rather than `layer`, so the rate limiter below can sit
        // outside it: refusing an over-quota caller is the cheapest answer this router
        // can give, and it should not be queued behind a deadline that exists to bound
        // work the caller is not going to be allowed to do.
        .route_layer(guardrails.timeout_layer())
        // `route_layer`, not `layer`: a request that matches no auth route should 404 without
        // spending anyone's quota. Nothing outside this nest is metered — in particular
        // `/healthz/*` must stay free, or a throttled probe restarts the pod under load.
        .route_layer(rate_limit::auth_limits::layer(auth_limit))
        .with_state(app_state.clone());

    // Explicit CORS Configurations:
    // - allow_origin: an explicit list; see `allowed_origins`. Never `Any` — this layer
    //   sits outside `.nest("/auth", ...)` so it governs the device-pairing endpoints
    //   too, and `allow_credentials(true)` makes a wildcard origin invalid regardless.
    // - allow_credentials: Set to true.
    // - allow_methods: Explicitly allow GET, POST, PUT, DELETE, OPTIONS.
    // - allow_headers: Explicitly allow Content-Type, Authorization, and X-Client-UUID.
    let cors = tower_http::cors::CorsLayer::new()
        .allow_origin(allowed_origins())
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
    let routed = Router::new()
        // No deadline on these three: each is a constant string with no I/O behind it,
        // so there is nothing for a timeout to bound.
        .route("/hello", get(|| async { "world" }))
        .route("/hellov2", get(|| async { "world2" }))
        // Superseded by `/healthz/live`. Kept until the cluster's probes have
        // been repointed, so this deploy cannot strand a rollout; delete after.
        .route("/healthcheck", get(|| async { "OK" }))
        // `/healthz/ready` does reach for Redis, so it does get one — a probe that can
        // hang forever is a probe that never reports unready.
        .merge(
            observability::health::health_routes(app_state.redis_client.clone())
                .layer(guardrails.timeout_layer()),
        )
        .nest("/api", api_routes)
        .nest("/auth", auth_routes)
        .layer(cors);

    // Read bottom-up: `Router::layer` makes the *last* call the outermost, so this
    // runs SetRequestId → track_request → Propagate → guardrails → CORS → routes.
    // The order matters and is not cosmetic — SetRequestId must precede the
    // middleware that logs the id, and Propagate must sit inside both so it sees the
    // stamped request on the way in and can copy the id onto the response on the way
    // out. `track_request` is outside `require_auth`, so rejected requests are
    // measured too.
    //
    // The guardrails go *inside* that trio for the same reason: the requests they
    // refuse — 413 for an oversized body, 503 for a shed one, 500 for a caught panic —
    // are exactly the ones an incident needs to see, and a request rejected outside
    // `track_request` would be invisible to the metrics and carry no request id. They
    // go *outside* CORS and everything below it because a panic guard that sits inside
    // the code it guards guards nothing. `guardrails.apply` documents its own internal
    // ordering.
    let app = guardrails
        .apply(routed)
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
