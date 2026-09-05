//! Pool sizing is a deployment-time decision that nothing else exercises: a wrong
//! number here does not fail a query, it just quietly reintroduces the five-slot
//! bottleneck. These tests pin the defaults that the module doc argues for, and the
//! fail-safe behaviour of the environment overrides.

use super::*;

#[test]
fn api_defaults_are_the_documented_numbers() {
    let config = PoolConfig::api_from_raw(None, None);

    assert_eq!(config.max_connections, 16);
    assert_eq!(config.acquire_timeout, Duration::from_secs(5));
    assert_eq!(config.idle_timeout, Some(Duration::from_secs(120)));
}

#[test]
fn api_acquire_timeout_is_far_below_the_sqlx_default() {
    // The whole point of setting it: 30s of queued waiting is what turns pool
    // starvation into an outage rather than shed load.
    assert!(PoolConfig::api_from_raw(None, None).acquire_timeout < Duration::from_secs(30));
}

#[test]
fn api_reads_both_overrides_from_the_environment() {
    let config = PoolConfig::api_from_raw(Some("32".to_string()), Some("9".to_string()));

    assert_eq!(config.max_connections, 32);
    assert_eq!(config.acquire_timeout, Duration::from_secs(9));
}

/// The environment is process-wide and `cargo test` runs tests in parallel, so every
/// assertion that needs `DATABASE_MAX_CONNECTIONS` or `DATABASE_ACQUIRE_TIMEOUT_SECS`
/// set lives in this one test rather than racing a sibling that sets them to
/// something else. It is the only place in the crate that mutates the environment.
#[test]
fn env_overrides_reach_the_api_pool_and_are_ignored_by_the_reaper() {
    let previous_max = std::env::var("DATABASE_MAX_CONNECTIONS").ok();
    let previous_timeout = std::env::var("DATABASE_ACQUIRE_TIMEOUT_SECS").ok();

    std::env::set_var("DATABASE_MAX_CONNECTIONS", "23");
    std::env::set_var("DATABASE_ACQUIRE_TIMEOUT_SECS", "7");
    let api = PoolConfig::api();
    // Sizing the request path is not permission to hand a sequential batch job the
    // same slice of Neon's connection budget.
    let reaper = PoolConfig::reaper();

    match previous_max {
        Some(value) => std::env::set_var("DATABASE_MAX_CONNECTIONS", value),
        None => std::env::remove_var("DATABASE_MAX_CONNECTIONS"),
    }
    match previous_timeout {
        Some(value) => std::env::set_var("DATABASE_ACQUIRE_TIMEOUT_SECS", value),
        None => std::env::remove_var("DATABASE_ACQUIRE_TIMEOUT_SECS"),
    }

    assert_eq!(api.max_connections, 23);
    assert_eq!(api.acquire_timeout, Duration::from_secs(7));
    assert_eq!(reaper.max_connections, 2);
    assert_eq!(reaper.acquire_timeout, Duration::from_secs(30));
}

#[test]
fn nonsense_overrides_fall_back_to_the_defaults() {
    // A manifest typo must not be able to size the pool down to nothing, which would
    // take every request out far more effectively than the bug this file fixes.
    for raw in ["0", "", "  ", "sixteen", "-4"] {
        assert_eq!(
            parse_max_connections(Some(raw.to_string()), 16),
            16,
            "max_connections should reject {raw:?}"
        );
        assert_eq!(
            parse_acquire_timeout(Some(raw.to_string()), Duration::from_secs(5)),
            Duration::from_secs(5),
            "acquire_timeout should reject {raw:?}"
        );
    }
}

#[test]
fn overrides_tolerate_surrounding_whitespace() {
    assert_eq!(parse_max_connections(Some(" 20 ".to_string()), 16), 20);
    assert_eq!(
        parse_acquire_timeout(Some(" 8 ".to_string()), Duration::from_secs(5)),
        Duration::from_secs(8)
    );
}

#[test]
fn the_reaper_gets_its_own_much_smaller_shape() {
    let reaper = PoolConfig::reaper();
    let api = PoolConfig::api_from_raw(None, None);

    assert!(reaper.max_connections < api.max_connections);
    // A batch job has nobody to shed load to, so it waits out a Neon wake-up instead
    // of failing fast the way a request path should.
    assert!(reaper.acquire_timeout > api.acquire_timeout);
    // Nothing to keep warm, and the process exits at the end of the sweep.
    assert_eq!(reaper.idle_timeout, None);
}

#[test]
fn idle_timeout_stays_inside_neons_autosuspend_window() {
    // If the pool held connections longer than Neon waits before suspending, the
    // compute would never scale to zero and a quiet service would bill all day.
    let idle = PoolConfig::api_from_raw(None, None)
        .idle_timeout
        .expect("the API pool sets an idle timeout");
    assert!(idle < Duration::from_secs(300));
}
