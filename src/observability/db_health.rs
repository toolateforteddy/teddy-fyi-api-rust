//! Passive Postgres health, inferred from traffic that already happened.
//!
//! Readiness cannot probe Postgres: Neon scales to zero and bills per wake-up,
//! so a timer-driven `SELECT 1` would keep the database awake around the clock
//! for monitoring alone. This module recovers the signal without spending a
//! single extra query — it watches the errors real requests already produce.
//!
//! Every `?` on a `sqlx::Error` in this codebase passes through
//! `impl From<sqlx::Error> for AppError`, so one hook there sees every database
//! failure the service experiences.
//!
//! # Three classes of error, three different meanings
//!
//! Treating "a database call failed" as "the database is down" would be wrong in
//! both directions, so failures are classified before they count:
//!
//! * **Answered** — a constraint violation, `RowNotFound`, a decode error. The
//!   server received the query and replied. That is *positive* evidence of
//!   connectivity, so it **clears** the failure streak. This is what makes the
//!   detector passive in both directions: successful round trips never reach
//!   this module, but answered errors do, and they are just as good a liveness
//!   signal. Without this, a client sending a payload that violates a unique
//!   index could drive a pod out of rotation.
//! * **Unreachable** — I/O, TLS, or a closed pool/connection. Nothing answered.
//!   This is the only class that counts toward unreadiness.
//! * **Saturated** — `PoolTimedOut` *with a populated pool*. Counted as
//!   degradation for the metric, but deliberately **not** toward unreadiness.
//!   Pool exhaustion is a load signal, and `max_connections` is 5; if load made
//!   every replica report unready at once, the load balancer would be left with
//!   no endpoints and a slowdown would become a total outage. Shedding traffic
//!   is the wrong response to being busy.
//!
//! # `PoolTimedOut` is the outage case too, which is why the pool is consulted
//!
//! Verified empirically, and it is the opposite of what the variant names
//! suggest: when Postgres is unreachable, sqlx does **not** surface
//! `Error::Io`. The pool absorbs the failed connect attempts and retries
//! internally until `acquire_timeout` elapses, so the caller sees
//! `PoolTimedOut` — the same variant as genuine load. Classifying on the error
//! alone would file every real outage under "saturated" and the detector would
//! never fire in the one situation it exists for.
//!
//! The two are told apart by whether the pool is **full**, read from in-memory
//! counters on the pool — **not** a query, so consulting it costs no Neon
//! wake-up:
//!
//! * `size() == max_connections` — the pool filled up and every connection is
//!   busy doing work. That is load. Saturated.
//! * `size() < max_connections` — the pool could not fill. Nothing is answering.
//!   Unreachable.
//!
//! Comparing against `max_connections` rather than against zero is deliberate
//! and was arrived at empirically: `size()` counts connections *being
//! established*, so during a failing acquire it is non-zero — a `== 0` test
//! reads as "saturated" throughout the entire outage, which is the bug this
//! replaces. It also drains to 0 only *after* the acquire gives up, so the first
//! failures of an outage see a stale count of live-but-dead connections.
//! Against `max_connections` both of those read correctly.
//!
//! The residual false negative: an outage while ≥ `max_connections` requests are
//! in flight can momentarily look full. That direction is the safe one — it
//! defers detection to the error-rate alert rather than pulling a healthy
//! replica out of rotation.
//!
//! # Why the streak decays instead of being reset by successes
//!
//! Successful queries do not pass through the error funnel, so there is no free
//! hook to reset the counter on success — and adding one would mean touching
//! every one of the ~100 query sites. Instead the streak is time-boxed: it only
//! counts if the failures are recent. A database that recovers stops producing
//! errors, the streak ages out, and the pod becomes ready again on its own. A
//! database that is still down fails the next request and re-arms it. A pod
//! receiving no traffic at all reports ready, which is correct — it has no
//! evidence of failure and has not spent a wake-up looking for one.

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::sync::OnceLock;

/// The pool, for **metadata only**. Registered at boot so [`classify`] can read
/// `size()` — an atomic counter maintained by the pool itself, never a query and
/// never a connection. Nothing in this module may run SQL; that is the whole
/// premise of the passive design.
static POOL: OnceLock<sqlx::PgPool> = OnceLock::new();

/// Registers the pool at startup. Idempotent; a second call is ignored.
pub fn register_pool(pool: sqlx::PgPool) {
    let _ = POOL.set(pool);
}

/// Whether the pool is at capacity, or `None` before registration (tests, and
/// the reaper job, which never builds one).
fn pool_is_full() -> Option<bool> {
    POOL.get()
        .map(|pool| pool.size() >= pool.options().get_max_connections())
}

/// Whether the pool currently holds an idle, established connection.
///
/// This is the recovery signal. Successful queries never reach this module — the
/// hook is on the error path — so without it a replica stays unready for the
/// full window after the database comes back, even while it is serving 200s. An
/// idle connection is proof of reachability: during an outage the pool drains to
/// zero (verified), and a connection sitting idle is one the pool established
/// and has not found to be broken. Reading it is an atomic load, not a query.
fn pool_has_idle() -> Option<bool> {
    POOL.get().map(|pool| pool.num_idle() > 0)
}

/// Consecutive `Unreachable` failures not yet aged out.
static CONSECUTIVE_FAILURES: AtomicU32 = AtomicU32::new(0);
/// Unix millis of the most recent `Unreachable` failure.
static LAST_FAILURE_MS: AtomicI64 = AtomicI64::new(0);

/// Failures needed before a replica calls itself unready. More than one, so a
/// single dropped connection — routine against a serverless database that
/// recycles connections — cannot flap the probe.
const DEFAULT_FAILURE_THRESHOLD: u32 = 3;
/// How recent those failures must be.
///
/// **This must comfortably exceed the pool's `acquire_timeout` (sqlx's default
/// is 30s).** During an outage every failing request burns a full acquire
/// timeout before it errors, so serially-retried failures arrive roughly one
/// acquire timeout apart. A window at or below that interval means each failure
/// ages out the previous one, the streak never passes 1, and the detector can
/// never fire — which is exactly what an end-to-end test against a stopped
/// Postgres showed at 30s. Four times the acquire timeout leaves room for three
/// serial failures plus slack.
const DEFAULT_WINDOW_MS: i64 = 120_000;

/// What one `sqlx::Error` says about the database's reachability.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DbSignal {
    /// The server replied. Connectivity is fine; the query was not.
    Answered,
    /// Nothing replied.
    Unreachable,
    /// The local pool ran dry before a connection came free.
    Saturated,
}

/// Classifies an error by what it proves about connectivity, using the live pool.
pub fn classify(err: &sqlx::Error) -> DbSignal {
    classify_with_pool_state(err, pool_is_full())
}

/// The testable core: pool state arrives as an argument.
///
/// `sqlx::Error::Database` is the important case to get right: it means Postgres
/// itself returned an error code, which requires a working connection.
pub fn classify_with_pool_state(err: &sqlx::Error, pool_is_full: Option<bool>) -> DbSignal {
    match err {
        sqlx::Error::Io(_)
        | sqlx::Error::Tls(_)
        | sqlx::Error::Protocol(_)
        | sqlx::Error::PoolClosed => DbSignal::Unreachable,
        // See the module docs: this variant covers both an outage and real load,
        // and only the pool can say which. With no pool registered, assume
        // saturation — the conservative direction, since a false "unreachable"
        // pulls a healthy replica out of rotation.
        sqlx::Error::PoolTimedOut => match pool_is_full {
            Some(false) => DbSignal::Unreachable,
            // Full pool, or no pool registered. Assume load — the conservative
            // direction, since a false "unreachable" pulls a healthy replica out
            // of rotation.
            _ => DbSignal::Saturated,
        },
        // Database, RowNotFound, ColumnNotFound, ColumnDecode, Decode,
        // Configuration, Migrate, and anything added later: the server answered,
        // or the failure is ours. Defaulting non-exhaustively to `Answered` is
        // the safe direction — a new variant misread as unreachable would take
        // pods out of rotation, while one misread as answered merely leaves
        // detection to the error-rate alert.
        _ => DbSignal::Answered,
    }
}

fn failure_threshold() -> u32 {
    std::env::var("DB_UNHEALTHY_AFTER_FAILURES")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_FAILURE_THRESHOLD)
}

fn window_ms() -> i64 {
    std::env::var("DB_UNHEALTHY_WINDOW_MS")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_WINDOW_MS)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Feeds one database error into the detector. Called from the `AppError`
/// conversion, so every failing query in the service lands here exactly once.
pub fn record_error(err: &sqlx::Error) {
    record_signal(classify(err), now_ms());
}

/// The testable core: everything time-dependent arrives as an argument.
pub fn record_signal(signal: DbSignal, now: i64) {
    match signal {
        DbSignal::Answered => {
            // Proof of a working round trip.
            CONSECUTIVE_FAILURES.store(0, Ordering::Relaxed);
        }
        DbSignal::Saturated => {
            metrics::counter!("db_connectivity_failures_total", "class" => "saturated")
                .increment(1);
        }
        DbSignal::Unreachable => {
            metrics::counter!("db_connectivity_failures_total", "class" => "unreachable")
                .increment(1);

            // A streak that has already aged out starts over rather than
            // accumulating across an outage from an hour ago.
            let last = LAST_FAILURE_MS.swap(now, Ordering::Relaxed);
            if last != 0 && now.saturating_sub(last) > window_ms() {
                CONSECUTIVE_FAILURES.store(1, Ordering::Relaxed);
            } else {
                CONSECUTIVE_FAILURES.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Whether this replica currently believes Postgres is unreachable.
pub fn is_degraded() -> bool {
    is_degraded_with(now_ms(), pool_has_idle())
}

/// The testable core of [`is_degraded`], with the pool's idle state supplied.
pub fn is_degraded_with(now: i64, pool_has_idle: Option<bool>) -> bool {
    let failures = CONSECUTIVE_FAILURES.load(Ordering::Relaxed);
    if failures < failure_threshold() {
        return false;
    }
    let last = LAST_FAILURE_MS.load(Ordering::Relaxed);
    if last == 0 || now.saturating_sub(last) > window_ms() {
        return false;
    }

    // Recovered: the pool has re-established a connection, so the database is
    // answering again regardless of what the recent failure streak says. Clear
    // it so the replica returns to rotation immediately rather than serving 503s
    // for the rest of the window while happily serving 200s to everyone else.
    if pool_has_idle == Some(true) {
        CONSECUTIVE_FAILURES.store(0, Ordering::Relaxed);
        return false;
    }

    true
}

/// Convenience for tests that do not care about pool state.
#[cfg(test)]
pub fn is_degraded_at(now: i64) -> bool {
    is_degraded_with(now, None)
}

/// Publishes the flag as a gauge. Called on the readiness path, which the
/// kubelet drives on a timer, so the gauge tracks the flag without needing a
/// timer of its own.
pub fn publish_gauge() {
    metrics::gauge!("db_connectivity_degraded")
        .set(if is_degraded() { 1.0 } else { 0.0 });

    // Pool saturation against `max_connections(5)`, which is low enough to be a
    // plausible bottleneck. Also the evidence behind the outage-vs-load call
    // above, so it is worth being able to see it directly.
    if let Some(pool) = POOL.get() {
        metrics::gauge!("db_pool_connections").set(pool.size() as f64);
        metrics::gauge!("db_pool_idle").set(pool.num_idle() as f64);
    }
}

/// Test-only reset. The detector is process-global, and tests share a process.
#[cfg(test)]
pub fn reset() {
    CONSECUTIVE_FAILURES.store(0, Ordering::Relaxed);
    LAST_FAILURE_MS.store(0, Ordering::Relaxed);
}
