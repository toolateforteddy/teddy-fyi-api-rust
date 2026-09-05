//! Concurrency caps for the long-lived `/api/sync/stream` SSE connections.
//!
//! # Why a cap exists at all
//!
//! Accounts are free — Google sign-in, or device pairing — and a stream is a
//! long-lived server-side allocation that a client gets for the price of one HTTP
//! request, so without a cap a single account can open streams in a loop until the
//! replica falls over.
//!
//! Each stream now costs a task, a buffered `broadcast` receiver and a config
//! snapshot query, but **not** a Redis connection: streams share one process-wide
//! pub/sub connection, see [`crate::routes::sync::fanout`]. That removed the worst
//! consequence — exhausting Redis `maxclients`, which would fail the Redis ping in
//! `GET /healthz/ready` on *every* replica and take the whole service out of
//! rotation — but it did not make streams free, and the sharpest remaining edge is
//! Postgres: every open costs a snapshot query against a pool of a few connections.
//!
//! Two caps, because one is not enough:
//!
//! * **Per user** — bounds the obvious abuse, one account opening streams in a
//!   loop, while leaving a real family's handful of tablets alone.
//! * **Per process** — bounds the same attack spread across many free accounts,
//!   which the per-user cap cannot see. It is also a genuine capacity limit: a
//!   replica has a finite amount of memory and a small database pool, whoever owns
//!   the streams holding them.
//!
//! Slots are held by [`StreamSlot`], an RAII guard modelled on
//! [`crate::observability::metrics::SseConnectionGuard`]: the stream's `map`
//! closure captures it, so it lives exactly as long as the connection and is
//! released on `Drop` — the only moment a client disconnect is observable here.
//! On the error paths that return before the stream is built (a failed fan-out
//! registration, a failed config snapshot) the same `Drop` runs as the local binding
//! falls out of scope, so no path can leak a slot.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Concurrent streams one account may hold. Generous for a real household: a
/// tablet, a second tablet, and a parent's phone running the cloud dashboard.
/// Clients open exactly one stream each and reconnect rather than stacking, so
/// anything past this is either a buggy client or an attempt to exhaust the
/// replica.
pub const DEFAULT_MAX_STREAMS_PER_USER: usize = 3;

/// Concurrent streams one replica will hold, across all accounts. Left where it
/// was when streams cost a Redis connection apiece: the number is no longer sized
/// against `maxclients`, but it is still a defensible ceiling for the memory and
/// the database pool a single replica has, and lowering it is a config change
/// (`SSE_MAX_STREAMS_TOTAL`) rather than a code one. A full replica answers 503 on
/// this one endpoint instead of degrading everything it serves.
pub const DEFAULT_MAX_STREAMS_TOTAL: usize = 1_000;

/// Env var overriding [`DEFAULT_MAX_STREAMS_PER_USER`].
const MAX_STREAMS_PER_USER_ENV: &str = "SSE_MAX_STREAMS_PER_USER";
/// Env var overriding [`DEFAULT_MAX_STREAMS_TOTAL`].
const MAX_STREAMS_TOTAL_ENV: &str = "SSE_MAX_STREAMS_TOTAL";

/// Why a stream was refused. The two are distinguished because they mean
/// different things to the caller: the account is over its own limit and should
/// stop, or the replica is full and the client should simply retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRefusal {
    /// This account already holds its maximum. Answered `429`.
    PerUser,
    /// This replica is at capacity, whoever owns the streams. Answered `503`.
    Global,
}

impl StreamRefusal {
    /// Stable, bounded metric label.
    fn label(self) -> &'static str {
        match self {
            StreamRefusal::PerUser => "per_user",
            StreamRefusal::Global => "global",
        }
    }
}

/// The live stream counts for this process.
///
/// # Choice of primitive
///
/// A `std::sync::Mutex` around a plain `HashMap`, not a `tokio::sync::Mutex` and
/// not two independent atomics.
///
/// * `std` over `tokio`: the lock is held for a map lookup and an integer bump
///   and is **never** held across an `.await`, so an async mutex buys nothing
///   here and costs a scheduler-aware lock on every connect.
/// * One lock covering both counters rather than an atomic apiece: admission has
///   to decide against the per-user *and* the global count together. With
///   separate atomics two connects can each observe room and both be admitted,
///   which is exactly the overshoot the global cap exists to prevent.
///
/// A poisoned lock is recovered from rather than propagated: nothing here can
/// leave the map inconsistent (the only writes are `+1`/`-1`), and refusing every
/// stream forever because some unrelated task panicked would be a worse outage
/// than the one this module prevents.
pub struct StreamSlots {
    counts: Mutex<Counts>,
    max_per_user: usize,
    max_total: usize,
}

#[derive(Default)]
struct Counts {
    /// Only accounts with at least one open stream appear; the entry is removed
    /// at zero, so an attacker cannot grow the map by cycling through user ids.
    per_user: HashMap<String, usize>,
    total: usize,
}

impl StreamSlots {
    /// Reads both caps from the environment, falling back to the constants above.
    /// Zero and unparseable values fall back too — an operator typo must not
    /// silently switch the endpoint off for everybody.
    pub fn from_env() -> Self {
        Self::with_limits(
            read_limit(MAX_STREAMS_PER_USER_ENV, DEFAULT_MAX_STREAMS_PER_USER),
            read_limit(MAX_STREAMS_TOTAL_ENV, DEFAULT_MAX_STREAMS_TOTAL),
        )
    }

    pub fn with_limits(max_per_user: usize, max_total: usize) -> Self {
        Self {
            counts: Mutex::new(Counts::default()),
            max_per_user,
            max_total,
        }
    }

    /// Claims one slot for `user_id`, or says why it cannot.
    ///
    /// The global cap is checked first: when the replica is full there is no
    /// capacity for anyone, and telling a well-behaved account that it is over
    /// *its own* limit would send it the wrong retry signal.
    pub fn try_acquire(self: &Arc<Self>, user_id: &str) -> Result<StreamSlot, StreamRefusal> {
        let mut counts = self.lock();

        if counts.total >= self.max_total {
            drop(counts);
            record_refusal(StreamRefusal::Global);
            return Err(StreamRefusal::Global);
        }

        let for_user = counts.per_user.entry(user_id.to_string()).or_insert(0);
        if *for_user >= self.max_per_user {
            drop(counts);
            record_refusal(StreamRefusal::PerUser);
            return Err(StreamRefusal::PerUser);
        }

        *for_user += 1;
        counts.total += 1;
        drop(counts);

        Ok(StreamSlot {
            slots: Arc::clone(self),
            user_id: user_id.to_string(),
        })
    }

    /// Streams currently held by one account. Diagnostics and tests.
    pub fn active_for_user(&self, user_id: &str) -> usize {
        self.lock().per_user.get(user_id).copied().unwrap_or(0)
    }

    /// Streams currently held by this process. Diagnostics and tests.
    pub fn active_total(&self) -> usize {
        self.lock().total
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Counts> {
        self.counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn release(&self, user_id: &str) {
        let mut counts = self.lock();
        counts.total = counts.total.saturating_sub(1);
        if let Some(for_user) = counts.per_user.get_mut(user_id) {
            *for_user = for_user.saturating_sub(1);
            if *for_user == 0 {
                counts.per_user.remove(user_id);
            }
        }
    }
}

impl Default for StreamSlots {
    fn default() -> Self {
        Self::from_env()
    }
}

/// Holds one stream's slot for as long as it is alive. Deliberately RAII rather
/// than an explicit close, because there is no close: the stream ends when the
/// client vanishes, and only the destructor reliably observes that.
pub struct StreamSlot {
    slots: Arc<StreamSlots>,
    user_id: String,
}

impl Drop for StreamSlot {
    fn drop(&mut self) {
        self.slots.release(&self.user_id);
    }
}

/// Counts a refused stream, so a cap actually being hit is visible on a dashboard
/// rather than looking like clients that quietly stopped connecting.
fn record_refusal(reason: StreamRefusal) {
    metrics::counter!("sse_streams_refused_total", "reason" => reason.label()).increment(1);
}

fn read_limit(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}
