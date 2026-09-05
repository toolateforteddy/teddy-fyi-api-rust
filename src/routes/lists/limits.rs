//! The numbers that bound `/api/lists/invite` and `/api/lists/join`.
//!
//! Both endpoints are authenticated, but an account here is free — Google sign-in, or
//! device pairing — so "authenticated" bounds nothing on its own. Two separate problems
//! live behind these two routes:
//!
//! * **Guessing.** A join code is eight uppercase alphanumerics. That is a ~2.8e12 space,
//!   which sounds comfortable until you notice that nothing made a wrong guess cost
//!   anything: no rate limit, no counter, and every outstanding invite is another live
//!   target. A hit is not a read-only leak — it inserts a `grocery_list_members` row with
//!   role `MEMBER`, so the guesser can also rewrite the family's list.
//! * **Row creation.** `invite_handler` minted a 24-hour row per call, forever, for any
//!   list the caller belongs to. One account, one loop, unbounded table.
//!
//! The shapes here are deliberately copied from [`crate::auth::device`] — its
//! `MAX_CLAIM_FAILURES` / `CLAIM_FAILURE_WINDOW_MINS` pair, its `device_claim_failures`
//! table, its `CODE_GENERATION_ATTEMPTS` bound. Two brute-force defences that look alike
//! are two that get reviewed, tuned and understood together.
//!
//! Every number is overridable from the environment, following `SSE_MAX_STREAMS_PER_USER`
//! and `CORS_ALLOWED_ORIGINS`: an incident should be survivable with a rollout restart
//! rather than a rebuild.

/// Failed joins, inside [`DEFAULT_JOIN_FAILURE_WINDOW_MINS`], after which an account is
/// refused outright.
///
/// Five, matching `MAX_CLAIM_FAILURES`, and for the same reason: a person joining a real
/// list is pasting or carefully typing a code somebody sent them, so one or two failures
/// is a bad transcription and five is somebody doing something else. At five per ten
/// minutes an attacker gets ~720 guesses a day per account against a ~2.8e12 space, and
/// codes only live 24 hours — the expected number of accounts they must create to land one
/// hit stays absurd.
pub const DEFAULT_MAX_JOIN_FAILURES: i64 = 5;

/// The window those failures are counted over. Ten minutes, as with claims: long enough
/// that a script cannot simply pace itself around it, short enough that a parent who
/// fumbled a code is not locked out for the evening.
pub const DEFAULT_JOIN_FAILURE_WINDOW_MINS: i64 = 10;

/// Failed redemptions of one *specific* code before that code is destroyed.
///
/// This is the per-row counter, the sibling of `device_authorizations.attempts`. It cannot
/// see a guess that matched nothing — that guess has no row to be counted against, which
/// is what the per-account counter above is for. What it does see is a code that exists
/// and is being refused, and a code somebody is still poking at after it stopped working
/// has no business staying around: past this many attempts the handler deletes it.
///
/// Three rather than five, because unlike a claim there is no honest reason to present the
/// same dead code repeatedly: the client is told the same thing every time.
pub const DEFAULT_MAX_INVITE_ATTEMPTS: i32 = 3;

/// Unexpired invites one account may have outstanding across all its lists.
///
/// An invite is a live credential to somebody else's data, so this is a security number as
/// much as a storage one: the cap is the size of the attack surface an account is allowed
/// to leave lying around. Ten covers the real case with room to spare — a household has a
/// handful of lists and invites a partner or a grandparent, one code at a time, and each
/// code dies on use or within 24 hours. Anything past ten in a single day is a loop.
///
/// Expired and consumed invites do not count: a code is deleted when it is redeemed, and
/// an expired one is swept, so a parent whose invite lapsed can immediately issue another.
pub const DEFAULT_MAX_OUTSTANDING_INVITES_PER_USER: i64 = 10;

/// Draws at a unique code before `invite_handler` gives up.
///
/// The original loop was unbounded: `SELECT` for a collision, retry forever. With enough
/// rows — or with a bug — that is a request that never returns while holding a pool
/// connection. Each draw is independent from a ~2.8e12 space, so eight failures in a row
/// means something is wrong with the table rather than with the dice, and a clean 500 is
/// the honest answer. Mirrors `CODE_GENERATION_ATTEMPTS`.
pub const INVITE_CODE_GENERATION_ATTEMPTS: usize = 8;

const MAX_JOIN_FAILURES_ENV: &str = "LIST_JOIN_MAX_FAILURES";
const JOIN_FAILURE_WINDOW_ENV: &str = "LIST_JOIN_FAILURE_WINDOW_MINS";
const MAX_INVITE_ATTEMPTS_ENV: &str = "LIST_INVITE_MAX_ATTEMPTS";
const MAX_OUTSTANDING_INVITES_ENV: &str = "LIST_MAX_OUTSTANDING_INVITES_PER_USER";

pub fn max_join_failures() -> i64 {
    read_positive(MAX_JOIN_FAILURES_ENV, DEFAULT_MAX_JOIN_FAILURES)
}

pub fn join_failure_window_mins() -> i64 {
    read_positive(JOIN_FAILURE_WINDOW_ENV, DEFAULT_JOIN_FAILURE_WINDOW_MINS)
}

pub fn max_invite_attempts() -> i32 {
    read_positive(MAX_INVITE_ATTEMPTS_ENV, DEFAULT_MAX_INVITE_ATTEMPTS as i64) as i32
}

pub fn max_outstanding_invites_per_user() -> i64 {
    read_positive(
        MAX_OUTSTANDING_INVITES_ENV,
        DEFAULT_MAX_OUTSTANDING_INVITES_PER_USER,
    )
}

/// Reads a limit from the environment, falling back to the compiled default.
///
/// Zero and unparseable values fall back too, exactly as `stream_limits::read_limit` does:
/// an operator typo must not silently turn a limit into "refuse everything" (a zero cap
/// would lock every family out of inviting) and must not silently disable it either. The
/// only way to change a number is to set it to a number.
fn read_positive(var: &str, default: i64) -> i64 {
    std::env::var(var)
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}
