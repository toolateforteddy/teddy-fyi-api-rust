//! Postgres connection pool construction, and the sizing decisions behind it.
//!
//! # Why the pool shape is a security concern, not just a tuning knob
//!
//! Every request that touches the database — sync, the SSE snapshot read, login,
//! device pairing — has to borrow a connection first. The pool is therefore the
//! narrowest shared resource in the service, and whatever number sits in
//! `max_connections` is the real concurrency limit of the whole API. It used to be
//! 5, with sqlx's default 30 second acquire timeout: a handful of slow or expensive
//! unauthenticated calls could occupy every slot, and every other request would then
//! sit in the acquire queue for half a minute — holding a tokio task and a client
//! socket the entire time — before failing. That converts "one slow endpoint" into
//! "the service is down", and it makes the queue itself the amplifier.
//!
//! The two changes here address exactly that: more slots so a burst has somewhere to
//! go, and a short acquire timeout so that when the slots do run out the service
//! sheds load quickly instead of accumulating half-minute-old waiters.
//!
//! # Why the numbers are what they are: Neon
//!
//! Production runs on Neon, whose serverless compute scales to zero and bills per
//! wake-up (the same property that shapes [`crate::observability::health`] and
//! [`crate::observability::db_health`]). Two consequences:
//!
//! * **There is a hard ceiling on connections and it is not large.** Neon's direct
//!   Postgres endpoint allows on the order of a hundred connections on a small
//!   compute, and that budget is shared by every replica *plus* the reaper CronJob.
//!   A per-process pool has to be multiplied by the replica count before it is
//!   compared against that ceiling, which is why the default here is deliberately
//!   modest rather than the 50-100 a dedicated Postgres would happily take. If the
//!   replica count ever grows past what `DEFAULT_API_MAX_CONNECTIONS × replicas` can
//!   afford, the fix is to point `DATABASE_URL` at Neon's PgBouncer pooler endpoint
//!   (the `-pooler` host), which multiplexes many client connections onto a few
//!   server ones — not to shrink this number back down.
//! * **Idle connections are not free.** Neon suspends the compute after a few
//!   minutes of inactivity, and a pool that holds connections open across that
//!   window keeps paying for compute nobody is using.
//!
//! # What is deliberately *not* set
//!
//! * **`min_connections` stays at 0.** Keeping a warm connection would take the cold
//!   start off the first request after a quiet period, which is genuinely nicer for
//!   the user — but it also means the pool is permanently attached to the database,
//!   the compute never suspends, and a service that today costs a few wake-ups a day
//!   starts billing around the clock. That is a product tradeoff, not a tuning knob,
//!   and for a low-traffic family-sync service the money wins: the first request of
//!   the morning may pay a wake-up, and everything behind it is warm.
//! * **`max_lifetime` keeps the sqlx default.** Nothing here has evidence about how
//!   Neon recycles connections server-side, and inventing a number would be
//!   cargo-culting.
//!
//! `idle_timeout` *is* set, and shortened, so the pool lets go before Neon's
//! autosuspend window rather than pinning the compute awake — see
//! [`DEFAULT_IDLE_TIMEOUT`].

use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

/// Connections the API server's pool may open, per process.
///
/// Sixteen, not five: this is one process serving sync, SSE snapshot reads, auth and
/// device pairing, and five slots make any single slow query a service-wide outage.
/// Sixteen is also small enough that several replicas plus the reaper stay under a
/// small Neon compute's connection ceiling without needing the pooler endpoint. It
/// is a default, not a law — `DATABASE_MAX_CONNECTIONS` overrides it.
const DEFAULT_API_MAX_CONNECTIONS: u32 = 16;

/// Connections the reaper job's pool may open.
///
/// The sweep is a single sequential pass; it has no concurrency to feed. Two rather
/// than one only so a query issued while a transaction is open cannot deadlock
/// against that transaction's own connection. Keeping this tiny matters because the
/// CronJob runs alongside the live replicas and draws on the same Neon budget.
const DEFAULT_REAPER_MAX_CONNECTIONS: u32 = 2;

/// How long a request waits for a free connection before giving up.
///
/// Five seconds, against sqlx's default of thirty. Under saturation the default
/// makes every caller hold a task and a socket for half a minute and *then* fail,
/// which is the worst of both worlds — the work is not done and the resources were
/// spent anyway. Failing fast turns pool exhaustion into shed load: the client gets
/// a 503 quickly and can back off.
///
/// Not lower than five, because a cold Neon compute has to wake before it can answer
/// the pool's first connect attempt, and a one-second budget would turn every
/// scale-from-zero into a burst of spurious 503s.
const DEFAULT_API_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// The reaper's acquire timeout, which is deliberately the opposite tradeoff.
///
/// A batch job has no client waiting on it and nothing to shed load *to*; giving up
/// early just means the sweep does not happen. Thirty seconds, so a Neon wake-up or
/// a moment of contention with live traffic is something it waits out.
const DEFAULT_REAPER_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long an unused connection is kept before the pool closes it.
///
/// Two minutes, comfortably inside Neon's autosuspend window, so a quiet service
/// actually goes quiet: the pool drains, the compute suspends, and the bill stops.
/// sqlx's ten-minute default would hold a connection open across most of the idle
/// periods this service has, keeping the compute awake for no work.
const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// The pool shape for one process. Built by [`PoolConfig::api`] or
/// [`PoolConfig::reaper`]; the server and the batch job want genuinely different
/// tradeoffs, so they are separate constructors rather than one set of numbers
/// stretched over both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolConfig {
    pub max_connections: u32,
    pub acquire_timeout: Duration,
    /// `None` leaves sqlx's default in place. Set for the API server; left unset for
    /// the reaper, which exits long before a connection could sit idle.
    pub idle_timeout: Option<Duration>,
}

impl PoolConfig {
    /// The API server's pool, with `DATABASE_MAX_CONNECTIONS` and
    /// `DATABASE_ACQUIRE_TIMEOUT_SECS` applied over the defaults above.
    pub fn api() -> Self {
        Self::api_from_raw(
            std::env::var("DATABASE_MAX_CONNECTIONS").ok(),
            std::env::var("DATABASE_ACQUIRE_TIMEOUT_SECS").ok(),
        )
    }

    /// The parsing half of [`PoolConfig::api`], split out so the rules are testable
    /// without mutating process-wide environment state — the same shape
    /// [`crate::jobs::reap_stale_users::ReapConfig`] uses.
    fn api_from_raw(max_connections: Option<String>, acquire_timeout_secs: Option<String>) -> Self {
        Self {
            max_connections: parse_max_connections(max_connections, DEFAULT_API_MAX_CONNECTIONS),
            acquire_timeout: parse_acquire_timeout(
                acquire_timeout_secs,
                DEFAULT_API_ACQUIRE_TIMEOUT,
            ),
            idle_timeout: Some(DEFAULT_IDLE_TIMEOUT),
        }
    }

    /// The reaper's pool. Deliberately ignores the API's environment variables: an
    /// operator raising `DATABASE_MAX_CONNECTIONS` is sizing the request path, and
    /// silently handing the same number to a batch job that needs two connections
    /// would spend the Neon connection budget on nothing.
    pub fn reaper() -> Self {
        Self {
            max_connections: DEFAULT_REAPER_MAX_CONNECTIONS,
            acquire_timeout: DEFAULT_REAPER_ACQUIRE_TIMEOUT,
            idle_timeout: None,
        }
    }
}

/// An unset, unparseable or zero value falls back to the default: a typo in a
/// manifest should not be able to configure the service down to no connections at
/// all, which would take every request out.
fn parse_max_connections(raw: Option<String>, default: u32) -> u32 {
    raw.and_then(|raw| raw.trim().parse::<u32>().ok())
        .filter(|max| *max > 0)
        .unwrap_or(default)
}

/// Same fail-safe rule as [`parse_max_connections`]. A zero-second acquire timeout
/// would reject every request that did not find an idle connection already waiting.
fn parse_acquire_timeout(raw: Option<String>, default: Duration) -> Duration {
    raw.and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(default)
}

/// Connects the pool without touching the schema. Split out from [`init_postgres`]
/// so the `reap-stale-users` job can reach the database without running migrations
/// of its own — it passes [`PoolConfig::reaper`] rather than the server's shape.
pub async fn connect_postgres(
    config: &PoolConfig,
) -> Result<sqlx::Pool<sqlx::Postgres>, Box<dyn std::error::Error>> {
    let database_url = std::env::var("DATABASE_URL")?;

    let mut options = PgPoolOptions::new()
        .max_connections(config.max_connections)
        .acquire_timeout(config.acquire_timeout);
    if let Some(idle_timeout) = config.idle_timeout {
        options = options.idle_timeout(idle_timeout);
    }

    Ok(options.connect(&database_url).await?)
}

/// The API server's pool, with any outstanding migrations applied.
pub async fn init_postgres() -> Result<sqlx::Pool<sqlx::Postgres>, Box<dyn std::error::Error>> {
    let config = PoolConfig::api();
    tracing::info!(
        max_connections = config.max_connections,
        acquire_timeout_secs = config.acquire_timeout.as_secs(),
        "Connecting Postgres pool"
    );
    let pool = connect_postgres(&config).await?;

    // FORCE RUN OUTSTANDING MIGRATIONS ON STARTUP
    // This looks at our local `/migrations` folder and updates Neon instantly
    sqlx::migrate!("./migrations").run(&pool).await?;

    println!("🚀 Database successfully synced and serverless migrations verified!");
    Ok(pool)
}

#[cfg(test)]
mod tests;
