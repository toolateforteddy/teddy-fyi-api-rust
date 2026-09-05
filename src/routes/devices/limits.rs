//! How many tablets one account may register.
//!
//! `POST /api/devices` takes a **client-chosen** `device_uuid` and, until this cap, would
//! insert a `devices` row for every distinct value it was handed. So would a sync request:
//! [`crate::routes::sync::device::ensure_device`] registers an unknown id on sight, which
//! is what lets a tablet appear without a separate registration call. One authenticated
//! account, a loop over fresh UUIDs, and the table grows without bound — and every row is
//! a scope that configs and drawings can then be written under, so the damage is not
//! confined to one table.
//!
//! The cap therefore lives at `ensure_device`, the single place a device row is created,
//! rather than on the REST handler. A cap on the handler alone would be decorative: the
//! sync path would still create devices for free.
//!
//! # The thing this must not do
//!
//! Lock a family out of a tablet they already own. Two rules keep that from happening, and
//! both are properties of where the check sits rather than of the number:
//!
//! * A `device_uuid` **already registered to the caller** never reaches the cap — that
//!   path returns before the count is taken. Re-registering is what a real tablet does on
//!   every launch, and it must keep working at, over, or far over the cap.
//! * A `device_uuid` registered to **another** account is still rejected as it always was,
//!   as a `403` and not a `429`. The two failures mean different things and the caller
//!   should be able to tell them apart.
//!
//! Only the creation of a *new* row is refused, and only once the account is already at
//! its limit.

/// Devices one account may have registered at once.
///
/// Sized from what the product is: a household with tablets for the kids, a parent's
/// phone running the cloud console, and the churn of a few years — a replaced tablet, a
/// factory reset that produced a fresh id, a device handed down. Ten is several times the
/// real household and still small enough that hitting it is a loop rather than a family.
///
/// There is deliberately no automatic eviction of the oldest device to make room. A
/// device row is what scopes a tablet's configs and drawings; silently retiring one would
/// silently orphan a child's saved work. A family at the cap should remove a device they
/// no longer use — a visible act — and the number is set high enough that this is rare.
pub const DEFAULT_MAX_DEVICES_PER_ACCOUNT: i64 = 10;

/// Env override, following `SSE_MAX_STREAMS_PER_USER` and `DATABASE_MAX_CONNECTIONS`: if
/// this number turns out to be wrong for a real family, support should be able to raise it
/// with a rollout restart rather than a release.
const MAX_DEVICES_PER_ACCOUNT_ENV: &str = "MAX_DEVICES_PER_ACCOUNT";

/// The cap in force. Zero and unparseable values fall back to the default rather than
/// being honoured: a typo that read as "zero devices" would stop every tablet on the
/// service from registering, which is a far worse outage than the one the cap prevents.
pub fn max_devices_per_account() -> i64 {
    std::env::var(MAX_DEVICES_PER_ACCOUNT_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_DEVICES_PER_ACCOUNT)
}
