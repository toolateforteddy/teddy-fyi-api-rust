//! Unauthenticated liveness and readiness endpoints for the Kubernetes probes.
//!
//! # Why readiness does not touch Postgres
//!
//! Neon scales its compute to zero and bills per wake-up, so a readiness probe
//! that ran `SELECT 1` every few seconds would keep the database awake around
//! the clock purely to answer the probe — for a low-traffic service that is the
//! dominant cost, and it would be spent on monitoring rather than on users.
//! Readiness therefore checks **Redis only**, which runs in-cluster and is free
//! to poll.
//!
//! Postgres health is instead inferred **passively**, from the errors real
//! requests already produce — see [`crate::observability::db_health`]. That
//! recovers most of what an active probe would tell us for zero extra queries.
//! The authenticated `/api/ready` still performs the deep database check for
//! on-demand, human-initiated use.
//!
//! Liveness is a static `OK` on purpose. Liveness failure means "kill this pod",
//! and a shared-dependency outage must never be able to restart every replica at
//! once — that turns a recoverable dependency blip into a full outage.

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde::Serialize;
use std::time::Duration;

/// Upper bound on the Redis round trip. Comfortably longer than a healthy
/// in-cluster PING, comfortably shorter than any sane probe `timeoutSeconds`,
/// so a hung connection reports unready instead of hanging the kubelet.
const REDIS_PING_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ReadyResponse {
    pub status: &'static str,
    /// Named so the failing dependency is visible in `kubectl describe` output
    /// and in the probe failure log, rather than requiring a second round of
    /// debugging to work out which check failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<&'static str>,
}

/// `GET /healthz/live` — the process is running and the runtime is scheduling.
pub async fn liveness_handler() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}

/// `GET /healthz/ready` — this replica can serve traffic.
///
/// Redis is the only dependency checked; see the module docs for why Postgres
/// is deliberately excluded.
pub async fn readiness_handler(State(redis_client): State<redis::Client>) -> impl IntoResponse {
    // The kubelet drives this on a timer, so it is also the cheapest place to
    // keep the gauge current without a ticker of its own.
    crate::observability::db_health::publish_gauge();

    // Checked before Redis: it costs nothing (a couple of atomic loads) and
    // Postgres being unreachable is the more serious of the two.
    if crate::observability::db_health::is_degraded() {
        tracing::warn!(
            "Readiness probe failed: postgres unreachable, inferred from recent request failures"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ReadyResponse {
                status: "unready",
                failed: Some("postgres"),
            }),
        );
    }

    match ping_redis(&redis_client).await {
        Ok(()) => (
            StatusCode::OK,
            Json(ReadyResponse {
                status: "ready",
                failed: None,
            }),
        ),
        Err(err) => {
            tracing::warn!("Readiness probe failed: redis: {}", err);
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ReadyResponse {
                    status: "unready",
                    failed: Some("redis"),
                }),
            )
        }
    }
}

/// Connects and PINGs, bounded by [`REDIS_PING_TIMEOUT`].
async fn ping_redis(redis_client: &redis::Client) -> Result<(), String> {
    let ping = async {
        let mut conn = redis_client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|err| format!("connect failed: {}", err))?;
        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .map_err(|err| format!("PING failed: {}", err))?;
        Ok(())
    };

    match tokio::time::timeout(REDIS_PING_TIMEOUT, ping).await {
        Ok(result) => result,
        Err(_) => Err(format!("timed out after {:?}", REDIS_PING_TIMEOUT)),
    }
}

/// The probe router.
///
/// Takes the Redis client rather than the full [`AppState`](crate::state::AppState)
/// so that the health path has no access to the database pool at all — the cost
/// constraint above is enforced by the type, not by a comment.
pub fn health_routes(redis_client: redis::Client) -> Router {
    Router::new()
        .route("/healthz/live", get(liveness_handler))
        .route("/healthz/ready", get(readiness_handler))
        .with_state(redis_client)
}
