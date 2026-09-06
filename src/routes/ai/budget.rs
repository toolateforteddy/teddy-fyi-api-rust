//! Spend limits for the two endpoints that call Gemini.
//!
//! # Why this exists
//!
//! `POST /api/categorize` and `POST /api/assign-icon` are the only endpoints in
//! this service that cost *money per request*: each one calls a billed Google API
//! with our `GEMINI_API_KEY`. Everything gating them was a valid session, and
//! sessions are free — anybody can obtain one with a Google sign-in or by pairing
//! a device. That makes the pair a denial-of-wallet: a single account looping on
//! `/api/assign-icon` bills us for as long as it cares to, and the first signal is
//! the invoice.
//!
//! So there are three separate brakes, because they fail in different ways:
//!
//! * **A per-user daily cap.** Bounds the obvious abuse — one account in a loop —
//!   while leaving a real household's usage untouched.
//! * **A global daily cap.** Bounds the same attack spread over many free
//!   accounts, which the per-user cap cannot see. It is the actual ceiling on the
//!   bill.
//! * **A kill switch.** Neither cap helps against something the caps did not
//!   anticipate (a pricing change, a runaway client rollout, a compromised key
//!   being rotated). This one is a Redis key so it can be flipped **live** —
//!   `redis-cli SET ai:gemini:disabled 1` — with no rebuild and no redeploy.
//!
//! # Counting, and what is counted
//!
//! Counters are `INCR`-ed **before** the upstream call, not after a successful
//! one. Google bills for requests that fail late as readily as for ones that
//! succeed, and a caller looping on requests that time out is exactly the shape of
//! abuse this is here to stop. Counting attempts is therefore the conservative
//! choice; it means a day of upstream errors also eats a user's budget, which is
//! acceptable for a feature this small.
//!
//! Keys carry the UTC date, which is what makes the budget "daily": at midnight
//! UTC every caller starts writing a fresh key that begins at zero. The TTL is
//! only garbage collection — nothing reads a key after its date has passed.
//!
//! # Behaviour when Redis is down
//!
//! Fails **closed**: a request that cannot be metered is refused rather than
//! passed upstream. A limit that evaporates in exactly the situation an attacker
//! can help bring about is not a limit. The cost of that choice is small and
//! bounded — `/api/categorize` and `/api/assign-icon` are conveniences, and a
//! replica that cannot reach Redis already fails `GET /healthz/ready` and leaves
//! rotation, so few requests reach this code in that state anyway.

use crate::routes::sync::types::AppError;

/// Gemini calls one account may make per UTC day.
///
/// Sized from what the feature is *for*: the client asks for a category once per
/// grocery item added and an icon once per todo created. A busy household adding
/// a full shop plus a week of chores is on the order of a hundred a day, so 200
/// leaves real usage a wide margin while cutting a scripted loop off in seconds.
pub const DEFAULT_MAX_CALLS_PER_USER_PER_DAY: u64 = 200;

/// Gemini calls this deployment will make per UTC day, across every account.
///
/// This is the number that bounds the bill. 20,000/day is ~100 accounts each at
/// their full per-user cap — far above real aggregate traffic for a service this
/// size, and at flash-lite pricing a few dollars a day rather than an incident.
/// Raise it when legitimate traffic approaches it, which the
/// `gemini_requests_total` metric shows well before the cap does.
pub const DEFAULT_MAX_CALLS_PER_DAY: u64 = 20_000;

/// Env var overriding [`DEFAULT_MAX_CALLS_PER_USER_PER_DAY`].
const MAX_CALLS_PER_USER_ENV: &str = "GEMINI_MAX_CALLS_PER_USER_PER_DAY";
/// Env var overriding [`DEFAULT_MAX_CALLS_PER_DAY`].
const MAX_CALLS_TOTAL_ENV: &str = "GEMINI_MAX_CALLS_PER_DAY";

/// Redis key whose mere presence stops all Gemini traffic. Deliberately a plain
/// string key with an obvious name: the point is that an on-call operator can
/// find and set it from `redis-cli` under pressure, without this repo open.
pub const KILL_SWITCH_KEY: &str = "ai:gemini:disabled";

/// How long a day's counter sticks around. Keys are already date-scoped, so this
/// is only cleanup; 25 hours covers the day itself plus clock skew between
/// replicas without keeping yesterday's keys around to confuse anyone reading
/// them by hand.
const COUNTER_TTL_SECS: i64 = 25 * 60 * 60;

/// The two caps, read once at the call site's convenience.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetLimits {
    pub per_user_per_day: u64,
    pub total_per_day: u64,
}

impl Default for BudgetLimits {
    fn default() -> Self {
        Self {
            per_user_per_day: DEFAULT_MAX_CALLS_PER_USER_PER_DAY,
            total_per_day: DEFAULT_MAX_CALLS_PER_DAY,
        }
    }
}

impl BudgetLimits {
    /// [`BudgetLimits::from_env`], read once per process.
    ///
    /// The caps are deployment configuration, not per-request input, and this
    /// sits in front of every AI request; re-reading the environment on each one
    /// buys nothing. Tests build limits directly rather than through here.
    pub fn cached() -> Self {
        static LIMITS: std::sync::OnceLock<BudgetLimits> = std::sync::OnceLock::new();
        *LIMITS.get_or_init(Self::from_env)
    }

    /// Reads both caps from the environment, falling back to the constants above.
    ///
    /// Zero and unparseable values fall back rather than being honoured: a typo in
    /// a manifest should not silently switch the feature off for everybody, and
    /// an operator who genuinely wants it off has the kill switch, which says so.
    pub fn from_env() -> Self {
        Self {
            per_user_per_day: read_limit(
                MAX_CALLS_PER_USER_ENV,
                DEFAULT_MAX_CALLS_PER_USER_PER_DAY,
            ),
            total_per_day: read_limit(MAX_CALLS_TOTAL_ENV, DEFAULT_MAX_CALLS_PER_DAY),
        }
    }
}

fn read_limit(var: &str, default: u64) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// Why a Gemini call was refused. Kept separate from [`AppError`] so the decision
/// is testable without going through a handler, and so each reason can pick its
/// own status code deliberately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetRefusal {
    /// The operator has turned Gemini off. Nothing is wrong with the request.
    KillSwitch,
    /// This account has spent its day's allowance.
    UserExhausted,
    /// The deployment has spent its day's allowance, whoever spent it.
    GlobalExhausted,
    /// The budget could not be read or written, so the call cannot be metered.
    Unmetered,
}

impl BudgetRefusal {
    /// Stable, bounded metric label.
    fn label(self) -> &'static str {
        match self {
            BudgetRefusal::KillSwitch => "kill_switch",
            BudgetRefusal::UserExhausted => "user_budget",
            BudgetRefusal::GlobalExhausted => "global_budget",
            BudgetRefusal::Unmetered => "unmetered",
        }
    }
}

impl From<BudgetRefusal> for AppError {
    fn from(refusal: BudgetRefusal) -> Self {
        match refusal {
            // 429 for both budget cases: the request is well-formed and the
            // caller is simply out of allowance until the next UTC day. A 429 is
            // the status clients already back off on, and it does not read as
            // "the server is broken" the way a 5xx would.
            BudgetRefusal::UserExhausted => AppError::TooManyRequests(
                "Daily AI request limit reached for this account; try again tomorrow".to_string(),
            ),
            BudgetRefusal::GlobalExhausted => AppError::TooManyRequests(
                "Daily AI request limit reached for this service; try again later".to_string(),
            ),
            // 503 rather than 429: this is a deliberate operator action, not the
            // caller's doing, and "temporarily unavailable" is the honest answer.
            BudgetRefusal::KillSwitch => {
                AppError::Overloaded("AI features are temporarily disabled".to_string())
            }
            // Also 503 — see the module docs on failing closed. The client should
            // retry; it did nothing wrong.
            BudgetRefusal::Unmetered => {
                AppError::Overloaded("AI features are temporarily unavailable".to_string())
            }
        }
    }
}

/// The UTC calendar day a counter belongs to, as `YYYY-MM-DD`.
fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn user_key(user_id: &str, day: &str) -> String {
    format!("ai:gemini:calls:user:{}:{}", user_id, day)
}

fn total_key(day: &str) -> String {
    format!("ai:gemini:calls:total:{}", day)
}

/// Charges one Gemini call to `user_id`, or says why it may not be made.
///
/// One round trip: the kill switch is read and both counters are bumped in a
/// single pipeline, because this sits in front of every AI request and a
/// three-round-trip check would cost more latency than the limit is worth. The
/// counters are still bumped when the kill switch is set; that is harmless (the
/// call is refused, and the keys expire) and it is what keeps this to one trip.
///
/// Not atomic across the three values in the strict sense — two concurrent
/// requests can both observe room at the boundary and both be admitted. `INCR`
/// itself is atomic, so the overshoot is bounded by in-flight concurrency, which
/// for a spend cap in the hundreds is noise.
pub async fn charge_gemini_call(
    redis_client: &redis::Client,
    limits: BudgetLimits,
    user_id: &str,
) -> Result<(), BudgetRefusal> {
    charge_gemini_call_on_day(redis_client, limits, user_id, &today()).await
}

/// [`charge_gemini_call`] against an explicit `YYYY-MM-DD`. The day is a
/// parameter rather than read inside so that "the budget resets daily" is
/// testable without waiting for midnight or mocking the clock.
pub(crate) async fn charge_gemini_call_on_day(
    redis_client: &redis::Client,
    limits: BudgetLimits,
    user_id: &str,
    day: &str,
) -> Result<(), BudgetRefusal> {
    let user_key = user_key(user_id, day);
    let total_key = total_key(day);

    let mut conn = match redis_client.get_multiplexed_async_connection().await {
        Ok(conn) => conn,
        Err(err) => {
            tracing::error!("Gemini budget: Redis connect failed: {:?}", err);
            crate::observability::metrics::record_redis_degraded("gemini_budget_connect");
            return Err(refuse(BudgetRefusal::Unmetered));
        }
    };

    let outcome: redis::RedisResult<(Option<String>, u64, u64)> = redis::pipe()
        .get(KILL_SWITCH_KEY)
        .incr(&user_key, 1)
        .expire(&user_key, COUNTER_TTL_SECS)
        .ignore()
        .incr(&total_key, 1)
        .expire(&total_key, COUNTER_TTL_SECS)
        .ignore()
        .query_async(&mut conn)
        .await;

    let (kill_switch, user_calls, total_calls) = match outcome {
        Ok(values) => values,
        Err(err) => {
            tracing::error!("Gemini budget: Redis metering failed: {:?}", err);
            crate::observability::metrics::record_redis_degraded("gemini_budget_incr");
            return Err(refuse(BudgetRefusal::Unmetered));
        }
    };

    if kill_switch_is_set(kill_switch.as_deref()) {
        tracing::warn!("Gemini call refused: kill switch is set");
        return Err(refuse(BudgetRefusal::KillSwitch));
    }
    if user_calls > limits.per_user_per_day {
        tracing::warn!(
            // Hashed like every other identifier that reaches the logs; the digest is
            // stable within a retention window, so "which account is burning the budget"
            // is still answerable from the log line alone.
            user_hash = %crate::observability::http::hash_user_id(
                user_id,
                &crate::observability::http::log_hash_salt_from_env(),
            ),
            calls = user_calls,
            limit = limits.per_user_per_day,
            "Gemini call refused: account over its daily budget"
        );
        return Err(refuse(BudgetRefusal::UserExhausted));
    }
    if total_calls > limits.total_per_day {
        tracing::error!(
            calls = total_calls,
            limit = limits.total_per_day,
            "Gemini call refused: deployment over its daily budget"
        );
        return Err(refuse(BudgetRefusal::GlobalExhausted));
    }

    Ok(())
}

/// Any value other than an explicit off means "disabled".
///
/// Deliberately lenient in that direction: an operator reaching for this key is
/// trying to stop spend, and `SET ai:gemini:disabled yes` must not be a no-op
/// because it was not the spelling this code expected. Only the deliberate
/// "off"/"0"/"false" spellings — or a missing key — leave Gemini enabled, so the
/// switch can be released without a `DEL`.
pub(crate) fn kill_switch_is_set(value: Option<&str>) -> bool {
    match value {
        None => false,
        Some(raw) => !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "false" | "off" | "no"
        ),
    }
}

/// Counts a refused call and hands the reason back, so a cap actually being hit
/// is visible on a dashboard rather than looking like clients that quietly
/// stopped asking.
fn refuse(reason: BudgetRefusal) -> BudgetRefusal {
    metrics::counter!("gemini_calls_refused_total", "reason" => reason.label()).increment(1);
    reason
}

/// Every `reason` label [`refuse`] can emit, so each series can be created at
/// zero at startup — "no data" and "no abuse" must not look alike.
pub const REFUSAL_REASONS: &[&str] = &["kill_switch", "user_budget", "global_budget", "unmetered"];
