//! Counters for the authentication surface: sign-in, refresh, and device pairing.
//!
//! These exist because `http_requests_total{route,status}` cannot answer any question
//! worth asking about authentication. Every interesting distinction here collapses into
//! one status code on purpose:
//!
//! * `/auth/login` answers `401` for an expired Google token *and* for a perfectly valid
//!   token whose audience this deployment has never heard of. The first is a user with a
//!   stale session; the second is a client ID missing from configuration — an outage for
//!   one whole app, invisible in the HTTP metrics.
//! * `/auth/refresh` deliberately collapses six internal outcomes into one `unauthorized`
//!   body ([`crate::auth::handlers::refresh_error`]) so an anonymous prober cannot
//!   enumerate sessions. That reasoning is about what the *caller* learns; the operator
//!   still needs the distinction, and this is where it goes.
//! * `/auth/device/claim` and `/auth/device/poll` answer `404` for unknown, expired and
//!   already-claimed codes alike, because telling them apart would let a caller sort real
//!   codes from invented ones (`context/2026-09-04_device_pairing_auth.md`). The server is
//!   the only place that distinction can exist at all.
//!
//! So the response codes are deliberately lossy and these counters are the compensating
//! channel. A refusal that is invisible to the caller by design must still be visible to
//! the person running the service, or the design is indistinguishable from a bug.
//!
//! # Label discipline
//!
//! Every label is a `&'static str` drawn from the arrays below, which is what keeps the
//! series count bounded and knowable: Prometheus cardinality is a function of the label
//! *values*, and one unbounded label is how a metrics bill becomes an incident. In
//! particular **no client ID is ever a label** — the accepted-audience set is a config
//! value that grows without a code change, so labelling by it would let editing a secret
//! silently multiply the series. The product a client belongs to is bounded, and is the
//! grouping anyone actually wants; the raw audience belongs in a log line.
//!
//! The arrays are also what [`crate::observability::metrics::register_baseline_metrics`]
//! iterates to emit every series at zero, so "nothing has failed" renders as `0` rather
//! than as absent. They live here, next to the call sites, so the two cannot drift.

use crate::auth::product::Product;

/// `POST /auth/login` outcomes.
pub const LOGIN_SUCCESS: &str = "success";
/// The Google ID token did not validate: expired, malformed, wrong signature.
pub const LOGIN_INVALID_TOKEN: &str = "invalid_token";
/// The token was genuine but its `aud` is not a configured client ID. Almost always
/// configuration, not attack — a new app build shipped with a client ID nobody added
/// here. Alert on this one.
pub const LOGIN_UNKNOWN_AUDIENCE: &str = "unknown_audience";
/// The sign-in failed on our side, after the caller was authenticated.
pub const LOGIN_ERROR: &str = "error";

pub const LOGIN_RESULTS: &[&str] = &[
    LOGIN_SUCCESS,
    LOGIN_INVALID_TOKEN,
    LOGIN_UNKNOWN_AUDIENCE,
    LOGIN_ERROR,
];

/// The `product` label for a path where no audience was ever resolved — a failed token,
/// or the `dev-auth` bypass, which names no Google client at all.
pub const PRODUCT_NONE: &str = "none";
/// An accepted audience carrying no product: see [`crate::auth::client_ids`]. Distinct
/// from [`PRODUCT_NONE`], and the distinction is the point — this one is the remaining
/// classification work, and watching it fall to zero is how you know the work is done.
pub const PRODUCT_UNCLASSIFIED: &str = "unclassified";

/// Every value the `product` label can take.
pub const PRODUCT_LABELS: &[&str] = &[
    "teddy_fyi",
    "scribbleroute",
    PRODUCT_UNCLASSIFIED,
    PRODUCT_NONE,
];

/// `POST /auth/refresh` outcomes. The endpoint answers `unauthorized` for all six
/// failures; these are the six.
pub const REFRESH_SUCCESS: &str = "success";
/// Rotated from the *old* hash inside the grace window — a retry racing its own
/// rotation. Succeeds, and is worth separating: a rising rate means clients are
/// retrying more than they should be.
pub const REFRESH_SUCCESS_IN_GRACE: &str = "success_in_grace";
/// No session row for this (`user_id`, `client_uuid`).
pub const REFRESH_NO_SESSION: &str = "no_session";
/// The right token, on a session past its expiry.
pub const REFRESH_EXPIRED: &str = "expired";
/// A genuinely issued token replayed after it was superseded. This is the reuse signal
/// rotation exists to catch, and the only one here that means *compromise* rather than
/// housekeeping. Alert on it.
pub const REFRESH_REUSE_OUTSIDE_GRACE: &str = "reuse_outside_grace";
/// The old hash matched but `rotated_at` was NULL — our own data-consistency bug, which
/// the handler logs at `error`. Should be flat at zero forever.
pub const REFRESH_ROTATED_AT_NULL: &str = "rotated_at_null";
/// A token matching neither hash: guessing, and the counter behind
/// [`crate::auth::handlers::FAILED_REFRESH_ALERT_THRESHOLD`].
pub const REFRESH_UNKNOWN_TOKEN: &str = "unknown_token";
/// Database or token-minting failure. The caller's token is probably still good.
pub const REFRESH_ERROR: &str = "error";

pub const REFRESH_RESULTS: &[&str] = &[
    REFRESH_SUCCESS,
    REFRESH_SUCCESS_IN_GRACE,
    REFRESH_NO_SESSION,
    REFRESH_EXPIRED,
    REFRESH_REUSE_OUTSIDE_GRACE,
    REFRESH_ROTATED_AT_NULL,
    REFRESH_UNKNOWN_TOKEN,
    REFRESH_ERROR,
];

/// `POST /auth/device/start` outcomes.
pub const DEVICE_START_SUCCESS: &str = "success";
/// A caller-supplied string over its length cap, rejected before it reached the database.
pub const DEVICE_START_INVALID_REQUEST: &str = "invalid_request";
/// The per-`client_uuid` cap on outstanding authorizations.
pub const DEVICE_START_CAPPED: &str = "capped";
pub const DEVICE_START_ERROR: &str = "error";

pub const DEVICE_START_RESULTS: &[&str] = &[
    DEVICE_START_SUCCESS,
    DEVICE_START_INVALID_REQUEST,
    DEVICE_START_CAPPED,
    DEVICE_START_ERROR,
];

/// `POST /auth/device/poll` outcomes. The tablet's side of pairing, and the one endpoint
/// here that is *supposed* to be mostly refusals: `pending` is the healthy steady state.
pub const DEVICE_POLL_PENDING: &str = "pending";
pub const DEVICE_POLL_AUTHORIZED: &str = "authorized";
pub const DEVICE_POLL_NOT_FOUND: &str = "not_found";
pub const DEVICE_POLL_EXPIRED: &str = "expired";
pub const DEVICE_POLL_RATE_LIMITED: &str = "rate_limited";
pub const DEVICE_POLL_ERROR: &str = "error";

pub const DEVICE_POLL_RESULTS: &[&str] = &[
    DEVICE_POLL_PENDING,
    DEVICE_POLL_AUTHORIZED,
    DEVICE_POLL_NOT_FOUND,
    DEVICE_POLL_EXPIRED,
    DEVICE_POLL_RATE_LIMITED,
    DEVICE_POLL_ERROR,
];

/// `POST /auth/device/claim` outcomes — the parent's side.
pub const DEVICE_CLAIM_SUCCESS: &str = "success";
pub const DEVICE_CLAIM_INVALID_TOKEN: &str = "invalid_token";
pub const DEVICE_CLAIM_UNKNOWN_AUDIENCE: &str = "unknown_audience";
/// Unknown, expired or already-claimed code — one label for what the caller is
/// deliberately told as one `404`.
pub const DEVICE_CLAIM_NOT_FOUND: &str = "not_found";
pub const DEVICE_CLAIM_RATE_LIMITED: &str = "rate_limited";
pub const DEVICE_CLAIM_ERROR: &str = "error";

pub const DEVICE_CLAIM_RESULTS: &[&str] = &[
    DEVICE_CLAIM_SUCCESS,
    DEVICE_CLAIM_INVALID_TOKEN,
    DEVICE_CLAIM_UNKNOWN_AUDIENCE,
    DEVICE_CLAIM_NOT_FOUND,
    DEVICE_CLAIM_RATE_LIMITED,
    DEVICE_CLAIM_ERROR,
];

/// The `product` label for a resolved audience.
///
/// Delegates to [`Product::as_wire`] rather than spelling the two names again, so the
/// dashboard label, the JWT claim and the `sessions.product` column cannot drift apart.
/// `None` here means *accepted but unclassified*, which is a real state with a real
/// consequence — see [`PRODUCT_UNCLASSIFIED`]. A path with no audience at all passes
/// [`PRODUCT_NONE`] explicitly instead of calling this.
pub fn product_label(product: Option<Product>) -> &'static str {
    product.map_or(PRODUCT_UNCLASSIFIED, Product::as_wire)
}

pub fn record_login(result: &'static str, product: &'static str) {
    metrics::counter!("auth_logins_total", "result" => result, "product" => product).increment(1);
}

pub fn record_refresh(result: &'static str) {
    metrics::counter!("auth_refreshes_total", "result" => result).increment(1);
}

/// One session crossing [`crate::auth::handlers::FAILED_REFRESH_ALERT_THRESHOLD`]
/// consecutive failed refreshes.
///
/// Separate from `auth_refreshes_total{result="unknown_token"}` because they answer
/// different questions: that counter rises with *volume* of guessing across the estate,
/// this one rises when a *single session* is being ground at, which is the shape of an
/// actual attack rather than a handful of stale clients.
pub fn record_refresh_bruteforce_alert() {
    metrics::counter!("auth_refresh_bruteforce_alerts_total").increment(1);
}

pub fn record_device_start(result: &'static str) {
    metrics::counter!("auth_device_starts_total", "result" => result).increment(1);
}

pub fn record_device_poll(result: &'static str) {
    metrics::counter!("auth_device_polls_total", "result" => result).increment(1);
}

pub fn record_device_claim(result: &'static str) {
    metrics::counter!("auth_device_claims_total", "result" => result).increment(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The metric label and the wire form are one string. `product.rs` documents why the
    /// claim and the column share `as_wire`; a third spelling on the dashboard would be
    /// the same bug with a slower feedback loop.
    #[test]
    fn the_product_label_is_the_wire_form() {
        for product in [Product::TeddyFyi, Product::ScribbleRoute] {
            assert_eq!(product_label(Some(product)), product.as_wire());
            assert!(
                PRODUCT_LABELS.contains(&product.as_wire()),
                "{} is emitted as a label but never registered at zero",
                product.as_wire()
            );
        }
    }

    #[test]
    fn an_unclassified_audience_is_not_the_same_label_as_no_audience() {
        assert_eq!(product_label(None), PRODUCT_UNCLASSIFIED);
        assert_ne!(PRODUCT_UNCLASSIFIED, PRODUCT_NONE);
        assert!(PRODUCT_LABELS.contains(&PRODUCT_UNCLASSIFIED));
        assert!(PRODUCT_LABELS.contains(&PRODUCT_NONE));
    }

    /// A duplicate in one of these arrays would emit the same series twice at zero, which
    /// is harmless, and would far more likely mean two outcomes were given one label,
    /// which is not.
    #[test]
    fn every_outcome_label_is_distinct_within_its_metric() {
        for labels in [
            LOGIN_RESULTS,
            PRODUCT_LABELS,
            REFRESH_RESULTS,
            DEVICE_START_RESULTS,
            DEVICE_POLL_RESULTS,
            DEVICE_CLAIM_RESULTS,
        ] {
            let mut seen = labels.to_vec();
            seen.sort_unstable();
            let before = seen.len();
            seen.dedup();
            assert_eq!(before, seen.len(), "duplicate label in {labels:?}");
        }
    }
}
