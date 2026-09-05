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
//!
//! # Why the Redis check is cached and its connection reused
//!
//! "Free to poll" is only true of a *PING*. Dialling is not free: the original
//! implementation called `get_multiplexed_tokio_connection()` on every probe, so
//! each replica opened, handshook and dropped a Redis connection every few
//! seconds forever. Worse, this endpoint is unauthenticated and reachable
//! through the ingress, so the rate is not actually bounded by the kubelet's
//! timer — anyone who can reach it can drive connection churn against the very
//! Redis whose exhaustion the SSE stream caps exist to prevent.
//!
//! Two changes remove that: a single multiplexed connection is held and reused
//! (redialled only when it breaks), and the *result* is cached for a TTL on the
//! order of the kubelet's own probe period. Together they make the cost of the
//! endpoint independent of how often it is called.
//!
//! Only the Redis leg is cached. `db_health::publish_gauge()` and
//! `db_health::is_degraded()` still run on **every** probe: they read atomics,
//! never a socket, so they cost nothing worth caching, and the kubelet's timer
//! remains the cheapest place to keep the `db_connectivity_degraded` gauge
//! current. A cache that short-circuited before them would silently freeze that
//! gauge; the ordering in [`readiness_at`] is what prevents it.

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use redis::aio::MultiplexedConnection;
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Upper bound on the Redis round trip. Comfortably longer than a healthy
/// in-cluster PING, comfortably shorter than any sane probe `timeoutSeconds`,
/// so a hung connection reports unready instead of hanging the kubelet.
const REDIS_PING_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a **healthy** Redis result is reused.
///
/// # What this cache is allowed to hide
///
/// Exactly one thing, and the bound is exactly this constant: if Redis dies
/// immediately after a successful check, this replica keeps answering `ready`
/// for at most `POSITIVE_TTL` before the next probe does real work. That is
/// well inside the noise of a kubelet that already needs several consecutive
/// failures (`failureThreshold`) spread over its own `periodSeconds` before it
/// removes an endpoint, so the cache cannot meaningfully delay a pod leaving
/// rotation — it can only collapse the redundant probes that arrive in between.
///
/// Chosen at the low end of the probe period so that in the normal case each
/// kubelet probe still performs a genuine PING; the TTL exists to absorb
/// *extra* traffic (multiple probe sources, and unauthenticated callers from the
/// ingress), not to skip the scheduled check.
const POSITIVE_TTL: Duration = Duration::from_millis(1_500);

/// How long a **failing** Redis result is reused.
///
/// # Why failures are cached at all, and why for much less time
///
/// The argument for caching successes only is that a cached failure delays
/// recovery being noticed. It is a real cost, so it is paid in the smallest
/// coin that still buys anything: a quarter of a second.
///
/// The argument for caching successes *only* sounds conservative but is the
/// dangerous one here. A failing check is the expensive one — a dead or
/// overloaded Redis makes every dial hang until [`REDIS_PING_TIMEOUT`] rather
/// than answering in microseconds — and this endpoint is unauthenticated and
/// reachable from the ingress. Leaving the failure path uncached means that
/// precisely when Redis is struggling, anything hitting `/healthz/ready` in a
/// loop gets to open an unbounded stream of connection attempts against it. That
/// is the outage amplifier this whole change exists to close, so the failure
/// path needs a cap too.
///
/// 250ms caps the dial rate at four per second per replica while costing at most
/// 250ms of extra unreadiness after Redis comes back — invisible against a
/// kubelet probe period measured in seconds. It never delays a *failure* being
/// noticed, only its clearing.
const NEGATIVE_TTL: Duration = Duration::from_millis(250);

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ReadyResponse {
    pub status: &'static str,
    /// Named so the failing dependency is visible in `kubectl describe` output
    /// and in the probe failure log, rather than requiring a second round of
    /// debugging to work out which check failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<&'static str>,
}

/// One cached Redis verdict and the instant it was produced.
#[derive(Clone)]
struct CachedCheck {
    checked_at: Instant,
    /// `Err` carries the rendered reason so a repeated failure still logs what
    /// actually went wrong rather than a bare "unready".
    result: Result<(), String>,
}

impl CachedCheck {
    /// A verdict is fresh for [`POSITIVE_TTL`] or [`NEGATIVE_TTL`] depending on
    /// which way it went; see those constants for the reasoning.
    fn is_fresh(&self, now: Instant, positive_ttl: Duration, negative_ttl: Duration) -> bool {
        let ttl = if self.result.is_ok() {
            positive_ttl
        } else {
            negative_ttl
        };
        now.saturating_duration_since(self.checked_at) < ttl
    }
}

/// The state behind `/healthz/ready`: a Redis client, the one connection it
/// keeps open, and the last verdict.
///
/// Deliberately owns a `redis::Client` and nothing else. It is **not** the
/// application state: giving the health path no access to the database pool is
/// what makes "readiness never queries Postgres" a property of the type rather
/// than of a comment somebody has to remember.
pub struct ReadinessProbe {
    client: redis::Client,
    /// The reused connection. `redis::aio::MultiplexedConnection` is a cheap
    /// `Clone` handle onto one shared socket, so the lock is only ever held for
    /// the clone — never across an `.await`, which is why a `std::sync::Mutex`
    /// is correct here. `None` means "not dialled yet, or the last attempt broke
    /// it"; either way the next check redials.
    conn: Mutex<Option<MultiplexedConnection>>,
    /// Last verdict, or `None` before the first check.
    cached: Mutex<Option<CachedCheck>>,
    /// Number of times a check has reached for the network — a dial, a PING, or
    /// both. Exists so tests can assert the cache is doing its job: the
    /// observable difference between a cached and an uncached probe is
    /// precisely "did we touch the socket". One check counts twice when a
    /// reused connection fails and is retried on a fresh one.
    network_attempts: AtomicU64,
    positive_ttl: Duration,
    negative_ttl: Duration,
}

impl ReadinessProbe {
    /// The production probe, with the TTLs above.
    pub fn new(client: redis::Client) -> Self {
        Self::with_ttls(client, POSITIVE_TTL, NEGATIVE_TTL)
    }

    /// Same, with explicit TTLs. Tests use this to exercise the TTL boundary
    /// without sleeping for real seconds.
    pub fn with_ttls(client: redis::Client, positive_ttl: Duration, negative_ttl: Duration) -> Self {
        Self {
            client,
            conn: Mutex::new(None),
            cached: Mutex::new(None),
            network_attempts: AtomicU64::new(0),
            positive_ttl,
            negative_ttl,
        }
    }

    /// How many times this probe has reached for the network since it was built.
    pub fn network_attempts(&self) -> u64 {
        self.network_attempts.load(Ordering::Relaxed)
    }

    /// The cached Redis check, as of `now`.
    ///
    /// `now` is a parameter rather than an `Instant::now()` inside so that the
    /// TTL boundary is testable without a real sleep.
    async fn check_redis_at(&self, now: Instant) -> Result<(), String> {
        if let Some(cached) = self.cached.lock().expect("readiness cache poisoned").clone() {
            if cached.is_fresh(now, self.positive_ttl, self.negative_ttl) {
                return cached.result;
            }
        }

        // The lock is released above, before this `.await`. Two probes racing
        // here both ping, which is fine: the window is one round trip, and the
        // alternative — holding a lock across the network call — would let a
        // hung Redis pile up probe handlers behind it.
        let result = self.ping_redis().await;

        *self.cached.lock().expect("readiness cache poisoned") = Some(CachedCheck {
            checked_at: now,
            result: result.clone(),
        });
        result
    }

    /// PINGs over the held connection, dialling first if there is none, bounded
    /// by [`REDIS_PING_TIMEOUT`].
    ///
    /// A failure on a *reused* connection is retried once on a fresh one. Redis
    /// closing an idle connection (a restart, its `timeout` setting, a rolled
    /// deployment) is routine and says nothing about whether Redis is up, so
    /// without the retry a healthy replica would report unready for one probe
    /// every time that happened.
    async fn ping_redis(&self) -> Result<(), String> {
        let attempt = async {
            let reused = self.take_conn();
            let was_reused = reused.is_some();

            match self.ping_with(reused).await {
                Ok(()) => Ok(()),
                Err(err) if was_reused => {
                    tracing::debug!("Readiness: redialling Redis after {}", err);
                    self.ping_with(None).await
                }
                Err(err) => Err(err),
            }
        };

        match tokio::time::timeout(REDIS_PING_TIMEOUT, attempt).await {
            Ok(result) => result,
            // The timeout covers the whole attempt, so whatever connection was
            // in flight is of unknown state — drop it so the next check redials.
            Err(_) => {
                self.clear_conn();
                Err(format!("timed out after {:?}", REDIS_PING_TIMEOUT))
            }
        }
    }

    /// One PING on `conn`, or on a freshly dialled connection when `None`.
    /// Stores the connection back for reuse on success, clears it on failure.
    async fn ping_with(&self, conn: Option<MultiplexedConnection>) -> Result<(), String> {
        // Counted before the dial, not after: a failing connect is exactly the
        // network work the cache exists to suppress, so it has to be visible.
        self.network_attempts.fetch_add(1, Ordering::Relaxed);

        let mut conn = match conn {
            Some(conn) => conn,
            None => self
                .client
                .get_multiplexed_tokio_connection()
                .await
                .map_err(|err| format!("connect failed: {}", err))?,
        };

        match redis::cmd("PING").query_async::<String>(&mut conn).await {
            Ok(_) => {
                // Keep the working connection for the next probe. This is the
                // whole point: in steady state the endpoint dials zero times.
                *self.conn.lock().expect("readiness conn poisoned") = Some(conn);
                Ok(())
            }
            Err(err) => {
                self.clear_conn();
                Err(format!("PING failed: {}", err))
            }
        }
    }

    fn take_conn(&self) -> Option<MultiplexedConnection> {
        self.conn.lock().expect("readiness conn poisoned").clone()
    }

    fn clear_conn(&self) {
        *self.conn.lock().expect("readiness conn poisoned") = None;
    }
}

/// `GET /healthz/live` — the process is running and the runtime is scheduling.
pub async fn liveness_handler() -> impl IntoResponse {
    (StatusCode::OK, "OK")
}

/// `GET /healthz/ready` — this replica can serve traffic.
///
/// Redis is the only dependency checked; see the module docs for why Postgres
/// is deliberately excluded, and for what the Redis cache is and is not allowed
/// to hide.
pub async fn readiness_handler(State(probe): State<Arc<ReadinessProbe>>) -> impl IntoResponse {
    readiness_at(&probe, Instant::now()).await
}

/// The testable core of [`readiness_handler`], with the clock supplied.
pub(crate) async fn readiness_at(
    probe: &ReadinessProbe,
    now: Instant,
) -> (StatusCode, Json<ReadyResponse>) {
    // Before any cache lookup, and unconditionally: the kubelet drives this on a
    // timer, so it is also the cheapest place to keep the gauge current without
    // a ticker of its own. Nothing below may be allowed to skip it — a cache
    // that short-circuited first would freeze the gauge at whatever it read when
    // the cache entry was written.
    crate::observability::db_health::publish_gauge();

    // Checked before Redis, and never cached: it costs nothing (a couple of
    // atomic loads), it is already exactly as fresh as the detector behind it,
    // and Postgres being unreachable is the more serious of the two.
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

    match probe.check_redis_at(now).await {
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

/// The probe router.
///
/// Takes the Redis client rather than the full [`AppState`](crate::state::AppState)
/// so that the health path has no access to the database pool at all — the cost
/// constraint above is enforced by the type, not by a comment.
pub fn health_routes(redis_client: redis::Client) -> Router {
    // One probe for the whole router, so the held connection and the cached
    // verdict are shared by every request rather than rebuilt per handler.
    let probe = Arc::new(ReadinessProbe::new(redis_client));

    Router::new()
        .route("/healthz/live", get(liveness_handler))
        .route("/healthz/ready", get(readiness_handler))
        .with_state(probe)
}
