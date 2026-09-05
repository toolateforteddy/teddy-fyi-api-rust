//! Per-IP rate limits for the unauthenticated `/auth/*` endpoints.
//!
//! Everything under `/auth` answers before any session exists, so none of it can be metered
//! per user — the caller has no identity yet. That makes the whole group free to hammer, and
//! one endpoint in it is expensive enough that hammering is a denial of service rather than a
//! nuisance: `/auth/device/start` runs an Argon2id hash (~19 MiB of memory and tens of
//! milliseconds of CPU per call) before it has decided anything about the caller. A few
//! hundred concurrent starts is enough to saturate CPU, drain the database pool behind them,
//! and push the pod into its memory limit.
//!
//! So there are two buckets, both keyed by client IP:
//!
//! * a general one over every `/auth/*` route, sized so that no realistic client notices it;
//! * a much tighter one on `/auth/device/start` alone, sized against the Argon2 cost.
//!
//! They stack — a `/auth/device/start` request draws from both — which is intended: the
//! tight bucket bounds the expensive work, the general one bounds everything else.

use crate::rate_limit::key::ClientIpKeyExtractor;
use governor::middleware::StateInformationMiddleware;
use std::sync::Arc;
use std::time::Duration;
use tower_governor::governor::{GovernorConfig, GovernorConfigBuilder};
use tower_governor::GovernorLayer;

/// The limiter state two layers can share: one bucket map, keyed by client IP.
pub type AuthRateLimitConfig = Arc<GovernorConfig<ClientIpKeyExtractor, StateInformationMiddleware>>;

/// The layer type both limiters produce.
///
/// `StateInformationMiddleware` is what adds `x-ratelimit-limit` / `x-ratelimit-remaining`
/// to successful responses; `retry-after` and `x-ratelimit-after` are on the 429 either way.
pub type AuthRateLimitLayer = GovernorLayer<ClientIpKeyExtractor, StateInformationMiddleware>;

/// Burst allowance for `/auth/*` as a group.
///
/// Sized for the worst legitimate case rather than the average one: a household behind a
/// single NAT address, with several tablets polling `/auth/device/poll` every five seconds
/// (the `interval` the pairing flow advertises) while a parent signs in on a phone. Thirty
/// requests of headroom covers that many times over and still caps a single address far
/// below what the pool can serve.
pub const AUTH_BURST: u32 = 30;

/// One token back every 500 ms — two requests per second sustained, per address.
///
/// Well above the ~0.2 req/s a paired tablet actually needs, and low enough that a single
/// address cannot hold the auth handlers busy.
pub const AUTH_REPLENISH_MS: u64 = 500;

/// Burst allowance for `/auth/device/start` alone.
///
/// Each call costs an Argon2id hash, so this number is really "how many 19 MiB hashes may
/// one address queue at once". Five is generous for the human flow behind it — a person
/// starting pairing on a tablet, mistyping, and starting again — and small enough that even
/// a large botnet needs a lot of distinct addresses to move the memory needle.
pub const DEVICE_START_BURST: u32 = 5;

/// One token back every 15 s — four starts a minute sustained, per address.
///
/// A code lives ten minutes (`CODE_TTL_SECS`), so a legitimate client wants roughly one
/// start per pairing attempt, not four a minute. Thirty times tighter than the general
/// bucket, which is the ratio the Argon2 cost argues for.
pub const DEVICE_START_REPLENISH_MS: u64 = 15_000;

/// Environment overrides, so an incident can be ridden out with a rollout restart instead of
/// a rebuild — the same escape hatch `CORS_ALLOWED_ORIGINS` and `RUST_LOG` already give.
const AUTH_BURST_ENV: &str = "AUTH_RATE_LIMIT_BURST";
const AUTH_REPLENISH_ENV: &str = "AUTH_RATE_LIMIT_REPLENISH_MS";
const DEVICE_START_BURST_ENV: &str = "DEVICE_START_RATE_LIMIT_BURST";
const DEVICE_START_REPLENISH_ENV: &str = "DEVICE_START_RATE_LIMIT_REPLENISH_MS";

/// One limiter's numbers, resolved from the environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quota {
    /// Requests one address may make back-to-back before it is throttled.
    pub burst: u32,
    /// How long until one of those requests is given back.
    pub replenish: Duration,
}

/// Reads an override, falling back to the compiled-in default.
///
/// A missing, empty, unparseable or zero value takes the default rather than panicking or
/// disabling the limiter: this is a knob turned under pressure, quite possibly at 3am, and a
/// fat-fingered value must not be the thing that takes the service down. Zero is rejected
/// because `GovernorConfigBuilder::finish` refuses it, and because "0" reads like "off" while
/// meaning "block everything".
pub(crate) fn positive_from_env(name: &str, default: u64) -> u64 {
    let raw = match std::env::var(name) {
        Ok(raw) => raw,
        Err(_) => return default,
    };
    match raw.trim().parse::<u64>() {
        Ok(value) if value > 0 => value,
        _ => {
            tracing::error!(
                "Ignoring unusable {} value {:?}; using default {}",
                name,
                raw,
                default
            );
            default
        }
    }
}

impl Quota {
    /// The quota for `/auth/*` as a group.
    pub fn general_auth() -> Self {
        Self::from_env(
            AUTH_BURST_ENV,
            AUTH_BURST,
            AUTH_REPLENISH_ENV,
            AUTH_REPLENISH_MS,
        )
    }

    /// The quota for `/auth/device/start`.
    pub fn device_start() -> Self {
        Self::from_env(
            DEVICE_START_BURST_ENV,
            DEVICE_START_BURST,
            DEVICE_START_REPLENISH_ENV,
            DEVICE_START_REPLENISH_MS,
        )
    }

    fn from_env(
        burst_env: &str,
        burst_default: u32,
        replenish_env: &str,
        replenish_default: u64,
    ) -> Self {
        let burst = positive_from_env(burst_env, burst_default as u64);
        Self {
            // Saturating rather than wrapping: an absurd override should clamp, not alias
            // down to a tiny burst that throttles every real client.
            burst: u32::try_from(burst).unwrap_or(u32::MAX),
            replenish: Duration::from_millis(positive_from_env(replenish_env, replenish_default)),
        }
    }

    /// Builds the shareable limiter state for this quota.
    ///
    /// Kept separate from [`layer`] so a caller can hold the config, hand clones of it to
    /// several layers, and pass it to [`spawn_key_gc`].
    pub fn config(self) -> AuthRateLimitConfig {
        let config = GovernorConfigBuilder::default()
            .key_extractor(ClientIpKeyExtractor)
            .use_headers()
            .period(self.replenish)
            .burst_size(self.burst)
            .finish()
            // Unreachable: `positive_from_env` guarantees both values are non-zero, which is
            // the only way `finish` returns `None`.
            .expect("rate limit quota must have a non-zero burst and period");
        Arc::new(config)
    }
}

/// Wraps limiter state in the tower layer that enforces it.
///
/// Takes an already-built config rather than a [`Quota`] so that two layers can share one
/// bucket map — building the config twice would quietly give a caller two separate quotas.
pub fn layer(config: AuthRateLimitConfig) -> AuthRateLimitLayer {
    GovernorLayer { config }
}

/// How often the per-IP bucket map is swept.
const GC_INTERVAL: Duration = Duration::from_secs(60);

/// Drops buckets for addresses that have gone quiet.
///
/// governor keys its state by IP and never forgets one on its own, so without this the map
/// grows once per distinct address seen — which, on the endpoints most attractive to a
/// spoofed-header flood, is its own slow memory leak. `retain_recent` keeps only buckets
/// that are not fully replenished; a dropped bucket is indistinguishable from one that has
/// recovered its whole quota, so this cannot let a throttled caller through early.
pub fn spawn_key_gc(config: AuthRateLimitConfig) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(GC_INTERVAL);
        loop {
            ticker.tick().await;
            config.limiter().retain_recent();
        }
    });
}
