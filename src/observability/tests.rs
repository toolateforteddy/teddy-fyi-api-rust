use crate::observability::health::{
    liveness_handler, readiness_handler, ReadinessProbe, ReadyResponse,
};
use crate::observability::http::hash_user_id;
use axum::{extract::State, http::StatusCode, response::IntoResponse};
use std::sync::Arc;

/// A probe pointed at a port nothing listens on.
///
/// Port 1 is reserved and never listening, so the failure path is exercised
/// deterministically — CI has no Redis, and these tests still have to run there.
fn unreachable_probe() -> Arc<ReadinessProbe> {
    Arc::new(ReadinessProbe::new(
        redis::Client::open("redis://127.0.0.1:1").expect("valid url"),
    ))
}

#[test]
fn hash_user_id_is_deterministic_and_salted() {
    let a = hash_user_id("user-1", "salt-a");
    assert_eq!(a, hash_user_id("user-1", "salt-a"), "same inputs must agree");

    // Without this the hash would be a plain digest of the id: an unsalted
    // SHA-256 over a small, guessable id space is trivially reversed by
    // anyone holding the logs, which would defeat the point of hashing at all.
    assert_ne!(a, hash_user_id("user-1", "salt-b"), "salt must change the digest");
    assert_ne!(a, hash_user_id("user-2", "salt-a"), "id must change the digest");
}

#[test]
fn hash_user_id_is_sixteen_hex_chars() {
    let hash = hash_user_id("user-1", "salt");
    assert_eq!(hash.len(), 16);
    assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
}

#[tokio::test]
async fn liveness_is_ok_without_touching_any_dependency() {
    assert_eq!(liveness_handler().await.into_response().status(), StatusCode::OK);
}

#[tokio::test]
async fn readiness_reports_unready_when_redis_is_unreachable() {
    let response = readiness_handler(State(unreachable_probe()))
        .await
        .into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn readiness_never_reaches_for_postgres() {
    // `readiness_handler` takes a `ReadinessProbe` — which owns a `redis::Client`
    // and nothing else — not `AppState`. Neon bills per
    // wake-up, so a probe that ran `SELECT 1` on a timer would keep the database
    // awake around the clock for monitoring alone. Postgres health comes from
    // `db_health` instead, which only ever reads atomics. The signature is the
    // guard; this test exists so that a future refactor to `State<AppState>` has
    // to delete an explicit statement of intent rather than silently regress it.
    let _: axum::response::Response = readiness_handler(State(unreachable_probe()))
        .await
        .into_response();
}

#[test]
fn ready_response_omits_the_failed_field_when_healthy() {
    let healthy = serde_json::to_value(ReadyResponse {
        status: "ready",
        failed: None,
    })
    .expect("serializes");
    assert_eq!(healthy, serde_json::json!({ "status": "ready" }));

    let unhealthy = serde_json::to_value(ReadyResponse {
        status: "unready",
        failed: Some("redis"),
    })
    .expect("serializes");
    assert_eq!(
        unhealthy,
        serde_json::json!({ "status": "unready", "failed": "redis" })
    );
}

mod db_health {
    use crate::observability::db_health::{
        classify_with_pool_state, is_degraded_at, is_degraded_with, record_signal, reset,
        DbSignal,
    };
    use std::sync::Mutex;

    /// The detector is process-global and `cargo test` runs these in parallel in
    /// one process, so without this they would clobber each other's counters.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Takes the lock and clears the detector. Recovers from poisoning so one
    /// failing test reports its own assertion rather than cascading into every
    /// other test in this module.
    ///
    /// Shared with the readiness cache tests: those go through `is_degraded()`
    /// and would otherwise race a db_health test into an unrelated
    /// `503 postgres` before the Redis leg is ever reached.
    pub(super) fn guard() -> std::sync::MutexGuard<'static, ()> {
        let lock = SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        reset();
        lock
    }

    const T0: i64 = 1_700_000_000_000;
    const WINDOW_MS: i64 = 120_000;

    #[test]
    fn unreachable_errors_are_classified_as_unreachable() {
        let io = sqlx::Error::Io(std::io::Error::other("connection reset"));
        assert_eq!(classify_with_pool_state(&io, Some(true)), DbSignal::Unreachable);
        assert_eq!(
            classify_with_pool_state(&sqlx::Error::PoolClosed, Some(true)),
            DbSignal::Unreachable
        );
    }

    #[test]
    fn a_server_that_answered_is_not_an_outage() {
        // `RowNotFound` shares the catch-all arm with `Error::Database`, which is
        // the case that matters: a constraint violation means Postgres received
        // the query and replied, so it is evidence of health, not of failure.
        assert_eq!(
            classify_with_pool_state(&sqlx::Error::RowNotFound, Some(false)),
            DbSignal::Answered
        );
    }

    #[test]
    fn a_pool_that_cannot_fill_turns_pool_timed_out_into_an_outage() {
        // Verified against a real stopped Postgres: sqlx reports `PoolTimedOut`,
        // never `Io`, because the pool absorbs the failed connects and retries
        // until `acquire_timeout`. Without this arm the detector could never fire
        // during an actual outage.
        assert_eq!(
            classify_with_pool_state(&sqlx::Error::PoolTimedOut, Some(false)),
            DbSignal::Unreachable
        );
    }

    #[test]
    fn a_full_pool_keeps_pool_timed_out_as_mere_load() {
        assert_eq!(
            classify_with_pool_state(&sqlx::Error::PoolTimedOut, Some(true)),
            DbSignal::Saturated
        );
    }

    #[test]
    fn an_unregistered_pool_assumes_load_rather_than_outage() {
        // The conservative direction: a false "unreachable" pulls a healthy
        // replica out of rotation, a false "saturated" only defers detection to
        // the error-rate alert.
        assert_eq!(
            classify_with_pool_state(&sqlx::Error::PoolTimedOut, None),
            DbSignal::Saturated
        );
    }

    #[test]
    fn one_failure_short_of_the_threshold_stays_ready() {
        let _g = guard();
        record_signal(DbSignal::Unreachable, T0);
        record_signal(DbSignal::Unreachable, T0 + 10);
        assert!(
            !is_degraded_at(T0 + 20),
            "a single dropped connection must not flap the probe"
        );
    }

    #[test]
    fn the_threshold_of_recent_failures_marks_the_replica_unready() {
        let _g = guard();
        for i in 0..3 {
            record_signal(DbSignal::Unreachable, T0 + i);
        }
        assert!(is_degraded_at(T0 + 10));
    }

    #[test]
    fn an_answered_query_clears_the_streak() {
        let _g = guard();
        for i in 0..3 {
            record_signal(DbSignal::Unreachable, T0 + i);
        }
        assert!(is_degraded_at(T0 + 10));

        // The guard against the worst false positive: a client whose payload
        // violates a unique index must not be able to drive pods out of rotation.
        record_signal(DbSignal::Answered, T0 + 11);
        assert!(!is_degraded_at(T0 + 12));
    }

    #[test]
    fn pool_exhaustion_never_marks_the_replica_unready() {
        let _g = guard();
        for i in 0..50 {
            record_signal(DbSignal::Saturated, T0 + i);
        }
        // If load could make every replica report unready at once, the load
        // balancer would be left with no endpoints and a slowdown would become a
        // total outage. Being busy is not a reason to shed traffic.
        assert!(!is_degraded_at(T0 + 60));
    }

    #[test]
    fn the_streak_ages_out_so_a_recovered_database_returns_to_rotation() {
        let _g = guard();
        let last_failure = T0 + 2;
        for i in 0..3 {
            record_signal(DbSignal::Unreachable, T0 + i);
        }
        assert!(is_degraded_at(T0 + 10));
        // The window runs from the most recent failure, not from the first.
        assert!(is_degraded_at(last_failure + WINDOW_MS));
        // No success ever reaches this module, so recovery has to be inferred
        // from the absence of further failures.
        assert!(!is_degraded_at(last_failure + WINDOW_MS + 1));
    }

    #[test]
    fn the_window_outlasts_the_pool_acquire_timeout() {
        let _g = guard();
        // The regression this encodes: during an outage each failure costs a full
        // `acquire_timeout` (sqlx default 30s), so serial retries land ~30s apart.
        // A window that did not outlast that interval would age out each failure
        // before the next arrived and the streak would never reach the threshold.
        const ACQUIRE_TIMEOUT_MS: i64 = 30_000;
        for i in 0..3 {
            record_signal(DbSignal::Unreachable, T0 + i * ACQUIRE_TIMEOUT_MS);
        }
        assert!(
            is_degraded_at(T0 + 2 * ACQUIRE_TIMEOUT_MS + 1),
            "three serially-retried failures must trip the flag"
        );
    }

    #[test]
    fn a_reconnected_pool_returns_the_replica_to_rotation_immediately() {
        let _g = guard();
        for i in 0..3 {
            record_signal(DbSignal::Unreachable, T0 + i);
        }
        assert!(is_degraded_with(T0 + 10, Some(false)), "still down");

        // The regression this encodes: successful queries never reach this
        // module, so without the pool's idle count a replica would keep serving
        // 503s for the rest of the window while already serving 200s to users.
        assert!(
            !is_degraded_with(T0 + 11, Some(true)),
            "an idle connection proves the database is answering again"
        );
    }

    #[test]
    fn a_stale_streak_restarts_instead_of_accumulating() {
        let _g = guard();
        record_signal(DbSignal::Unreachable, T0);
        record_signal(DbSignal::Unreachable, T0 + 1);

        // Hours later. Two failures from a long-finished incident must not
        // combine with one fresh failure to trip the threshold.
        let later = T0 + WINDOW_MS * 100;
        record_signal(DbSignal::Unreachable, later);
        assert!(!is_degraded_at(later + 1));
    }
}

/// The `/healthz/ready` Redis cache.
///
/// Every test here points at port 1 (nothing listening), so the Redis leg always
/// fails. That is deliberate: CI has no Redis, and a test that silently asserts
/// nothing when the dependency is absent is worse than no test. The cache
/// behaves identically either way — only which TTL applies differs — and
/// `network_attempts()` measures the thing that actually matters, which is
/// whether the probe reached for a socket at all.
mod readiness_cache {
    use crate::observability::health::{readiness_at, ReadinessProbe};
    use axum::http::StatusCode;
    use std::time::{Duration, Instant};

    /// A probe with explicit TTLs, so a test can step across the boundary on a
    /// supplied clock rather than depending on how long the assertions took.
    fn probe(positive: Duration, negative: Duration) -> ReadinessProbe {
        ReadinessProbe::with_ttls(
            redis::Client::open("redis://127.0.0.1:1").expect("valid url"),
            positive,
            negative,
        )
    }

    /// One probe, on a runtime local to the caller's thread.
    ///
    /// These are plain `#[test]`s rather than `#[tokio::test]`s for two
    /// reasons: the db_health serialisation guard must be held across the
    /// checks without being held across an `.await`, and the gauge test needs
    /// the whole future to run on the thread where `with_local_recorder`
    /// installed its thread-local recorder.
    fn check(probe: &ReadinessProbe, now: Instant) -> StatusCode {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(readiness_at(probe, now)).0
    }

    #[test]
    fn a_second_probe_inside_the_ttl_does_not_touch_redis() {
        let _guard = super::db_health::guard();
        let probe = probe(Duration::from_secs(10), Duration::from_secs(10));
        let t0 = Instant::now();

        assert_eq!(check(&probe, t0), StatusCode::SERVICE_UNAVAILABLE);
        let first = probe.network_attempts();
        assert!(first > 0, "the first probe must actually check Redis");

        // The point of the whole change: the kubelet drives this endpoint
        // forever and anyone on the ingress can drive it faster, so a repeat
        // call inside the TTL must cost nothing on the network.
        assert_eq!(
            check(&probe, t0 + Duration::from_secs(1)),
            StatusCode::SERVICE_UNAVAILABLE,
            "the cached verdict is reused"
        );
        assert_eq!(
            probe.network_attempts(),
            first,
            "a cached probe must not dial or PING"
        );
    }

    #[test]
    fn a_probe_after_the_ttl_checks_again() {
        let _guard = super::db_health::guard();
        let probe = probe(Duration::from_secs(10), Duration::from_secs(10));
        let t0 = Instant::now();

        check(&probe, t0);
        let first = probe.network_attempts();

        // The cache bounds staleness, it does not replace the check: once the
        // TTL has passed the verdict has to be earned again.
        check(&probe, t0 + Duration::from_secs(11));
        assert!(
            probe.network_attempts() > first,
            "an expired entry must be re-checked, not served stale forever"
        );
    }

    #[test]
    fn a_failure_is_cached_only_for_the_short_negative_ttl() {
        let _guard = super::db_health::guard();
        // Failures are cached — an unauthenticated flood against a dead Redis
        // must not amplify into unbounded dials — but for far less time than a
        // success, so recovery is noticed almost immediately.
        let probe = probe(Duration::from_secs(10), Duration::from_millis(250));
        let t0 = Instant::now();

        check(&probe, t0);
        let first = probe.network_attempts();

        check(&probe, t0 + Duration::from_millis(200));
        assert_eq!(
            probe.network_attempts(),
            first,
            "a failure is cached, so a flood cannot amplify into dials"
        );

        check(&probe, t0 + Duration::from_millis(300));
        assert!(
            probe.network_attempts() > first,
            "the negative TTL must be short enough to notice recovery quickly"
        );
    }

    #[test]
    fn the_db_health_gauge_is_published_on_every_probe_even_a_cached_one() {
        let _guard = super::db_health::guard();
        let recorder = super::gauge_spy::GaugeSpy::default();
        let sets = recorder.handle();

        let probe = probe(Duration::from_secs(10), Duration::from_secs(10));
        let t0 = Instant::now();

        metrics::with_local_recorder(&recorder, || {
            check(&probe, t0);
            check(&probe, t0 + Duration::from_secs(1));
        });

        assert_eq!(
            probe.network_attempts(),
            1,
            "the second probe was served from the cache"
        );
        // `publish_gauge()` runs before the cache is consulted precisely so the
        // gauge keeps tracking the kubelet's timer. If a future refactor moves
        // the cache lookup first, this drops to 1 and the gauge silently freezes
        // at whatever it read when the entry was written.
        assert_eq!(
            sets.count("db_connectivity_degraded"),
            2,
            "the gauge must be republished on a cached probe too"
        );
    }
}

/// A `metrics::Recorder` that counts `gauge.set()` calls per key.
///
/// Hand-rolled because `metrics-util`'s debugging recorder is only a transitive
/// dependency here, and promoting it to a direct one to count two calls is not
/// worth the extra supply-chain surface.
mod gauge_spy {
    use metrics::{
        Counter, CounterFn, Gauge, GaugeFn, Histogram, HistogramFn, Key, KeyName, Metadata,
        Recorder, SharedString, Unit,
    };
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Default, Clone)]
    pub(super) struct Sets(Arc<Mutex<HashMap<String, u64>>>);

    impl Sets {
        pub(super) fn count(&self, key: &str) -> u64 {
            self.0
                .lock()
                .expect("spy poisoned")
                .get(key)
                .copied()
                .unwrap_or(0)
        }
    }

    /// One gauge handle, bound to the key it was registered under.
    struct SpyGauge {
        key: String,
        sets: Sets,
    }

    impl GaugeFn for SpyGauge {
        fn set(&self, _value: f64) {
            *self
                .sets
                .0
                .lock()
                .expect("spy poisoned")
                .entry(self.key.clone())
                .or_insert(0) += 1;
        }
        fn increment(&self, _value: f64) {}
        fn decrement(&self, _value: f64) {}
    }

    /// Counters and histograms are not what these tests are about, so they get
    /// handles that do nothing rather than a second set of bookkeeping.
    struct Ignored;
    impl CounterFn for Ignored {
        fn increment(&self, _value: u64) {}
        fn absolute(&self, _value: u64) {}
    }
    impl HistogramFn for Ignored {
        fn record(&self, _value: f64) {}
    }

    #[derive(Default)]
    pub(super) struct GaugeSpy {
        sets: Sets,
    }

    impl GaugeSpy {
        pub(super) fn handle(&self) -> Sets {
            self.sets.clone()
        }
    }

    impl Recorder for GaugeSpy {
        fn describe_counter(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_gauge(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}
        fn describe_histogram(&self, _: KeyName, _: Option<Unit>, _: SharedString) {}

        fn register_counter(&self, _key: &Key, _: &Metadata<'_>) -> Counter {
            Counter::from_arc(Arc::new(Ignored))
        }

        fn register_gauge(&self, key: &Key, _: &Metadata<'_>) -> Gauge {
            Gauge::from_arc(Arc::new(SpyGauge {
                key: key.name().to_string(),
                sets: self.sets.clone(),
            }))
        }

        fn register_histogram(&self, _key: &Key, _: &Metadata<'_>) -> Histogram {
            Histogram::from_arc(Arc::new(Ignored))
        }
    }
}
