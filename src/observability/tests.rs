use crate::observability::health::{liveness_handler, readiness_handler, ReadyResponse};
use crate::observability::http::hash_user_id;
use axum::{extract::State, http::StatusCode, response::IntoResponse};

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
    // Port 1 is reserved and never listening, so this exercises the failure path
    // deterministically — unlike the cache tests, which silently assert nothing
    // when Redis happens to be down. CI has no Redis, and this test still runs.
    let client = redis::Client::open("redis://127.0.0.1:1").expect("valid url");

    let response = readiness_handler(State(client)).await.into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn readiness_never_reaches_for_postgres() {
    // `readiness_handler` takes a `redis::Client`, not `AppState`. Neon bills per
    // wake-up, so a probe that ran `SELECT 1` on a timer would keep the database
    // awake around the clock for monitoring alone. Postgres health comes from
    // `db_health` instead, which only ever reads atomics. The signature is the
    // guard; this test exists so that a future refactor to `State<AppState>` has
    // to delete an explicit statement of intent rather than silently regress it.
    let client = redis::Client::open("redis://127.0.0.1:1").expect("valid url");
    let _: axum::response::Response = readiness_handler(State(client)).await.into_response();
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
    fn guard() -> std::sync::MutexGuard<'static, ()> {
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

mod sse_device_targeting {
    use crate::routes::sync::publisher::SyncSseEvent;
    use crate::routes::sync::stream::event_targets_device;
    use uuid::Uuid;

    #[test]
    fn initial_state_and_unscoped_events_target_all_listeners() {
        let device_a = Uuid::new_v4();
        let initial = SyncSseEvent::InitialState {
            entity: "config".to_string(),
            data: serde_json::json!({}),
        };
        assert!(event_targets_device(&initial, None));
        assert!(event_targets_device(&initial, Some(device_a)));

        let unscoped_update = SyncSseEvent::Invalidate {
            entity: "config".to_string(),
            sender_client_id: None,
            device_uuid: None,
        };
        assert!(event_targets_device(&unscoped_update, None));
        assert!(event_targets_device(&unscoped_update, Some(device_a)));
    }

    #[test]
    fn device_scoped_events_only_target_matching_device() {
        let device_a = Uuid::new_v4();
        let device_b = Uuid::new_v4();
        let scoped_update = SyncSseEvent::DirectUpdate {
            entity: "config".to_string(),
            key: "theme".to_string(),
            value: serde_json::json!("dark"),
            sender_client_id: None,
            device_uuid: Some(device_a),
        };

        assert!(event_targets_device(&scoped_update, Some(device_a)));
        assert!(!event_targets_device(&scoped_update, Some(device_b)));
        assert!(!event_targets_device(&scoped_update, None));
    }
}
