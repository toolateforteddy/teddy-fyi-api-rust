//! Who wins a sync conflict, and where a row's `version` number comes from.
//!
//! # The policy
//!
//! **The write that reaches the server last wins, and the server alone decides the
//! version number a row carries.**
//!
//! Every synced table (`drawings`, `configs`, `grocery_*`, `todo_*`) carries a
//! monotonically increasing `version` that clients echo back on their next write. Two
//! decisions hang off it, and until this module existed both of them were made from
//! numbers the client chose:
//!
//! * *Which of two competing writes lands.* `drawings` and `configs` compared the
//!   client's `last_modified` — a millisecond stamp read off the tablet's own clock and
//!   sent in the request body — against the server's, and let the larger one overwrite
//!   the other. A device whose clock is wrong by a year (a factory-reset tablet before
//!   NTP lands, a child playing with the date picker) wins *every* conflict on the
//!   account until the clock is fixed, and a client that simply sets the field to
//!   `i64::MAX` wins them permanently and on purpose. The losing side is another
//!   device's drawings and settings, silently overwritten.
//! * *What the row's next version is.* The grocery and todo processors used
//!   `max(server_version, client_version) + 1`, so one request carrying
//!   `version: 2_000_000_000` moved that row's counter there for good — no correctness
//!   argument for it, and a short walk to the `i32` ceiling, where `+ 1` overflows and
//!   (in debug) panics.
//!
//! Both are now decided from server state only. The server's own row is the sole input
//! to [`advance_version`]; the client's number is consulted exactly once, when there is no
//! row yet, and is bounded there too ([`seed_version`]).
//!
//! # Why "last to arrive", rather than "reject the stale writer"
//!
//! With the client's clock out of the picture there is no honest way to compare *when*
//! two edits happened — the server sees only when they arrived. That leaves two
//! coherent choices, and this one is deliberate:
//!
//! * *Reject a client whose version is behind* would make the server-side row
//!   authoritative and drop the incoming edit. A tablet that was edited on a plane and
//!   syncs an hour later would have that work refused, and there is nowhere for a child's
//!   drawing to go once the server says no. Data loss for the honest offline case is a
//!   worse failure than the one being fixed.
//! * *Accept it, and let arrival order break the tie* — what this module does. A
//!   legitimate offline edit still lands when the device reconnects, and every other
//!   device learns about it on its next sync, because the write is stamped with the
//!   server's clock and so sorts after the cursor those devices hold.
//!
//! The cost is the familiar last-write-wins one: two devices editing the same row offline
//! keep only the edit that syncs second. That was already true; what changes is that the
//! winner is decided by something no client can forge, and that no client can install
//! itself as the permanent winner of every future conflict.
//!
//! This is also what the grocery and todo subsystems already did in effect — they never
//! rejected a write — so all five now share one rule rather than three.
//!
//! # The other client-supplied timestamps
//!
//! `last_modified` on `drawings`/`configs` had to become server-authoritative for a
//! second reason beyond conflicts: it *is* the download cursor
//! (`WHERE last_modified > $last_synced_ms`). A row stamped in the future is replayed to
//! every device on every sync forever; a row stamped in the past is never handed to the
//! sibling tablet at all. See `crate::routes::sync::config` and
//! `crate::routes::sync::drawing`, which now write the request's server timestamp and
//! keep the client's claim in the separate `client_last_modified` column.
//!
//! `created_at` (drawings, todo items, grocery items/lists) and `joined_at`
//! (`grocery_list_members`) are **not** in the same position and are deliberately left
//! alone. Nothing on the server compares them, orders by them, or filters a sync on them;
//! they are descriptive values the owning client reads back and shows. A client that
//! writes nonsense there misorders its own list display and nobody else's, and it cannot
//! use them to overwrite another device's row. Bounding them would cost a wire break for
//! no security gain.

use crate::routes::sync::types::AppError;

/// The largest `version` a row may reach before writes to it are refused.
///
/// `version` is an `i32` in every synced table, so the real wall is `i32::MAX`
/// (2_147_483_647) and crossing it is an overflow: a wrap in release, a panic in debug.
/// Stopping short of it turns that into a clean, explainable refusal.
///
/// The number is unreachable by legitimate use — it is two billion accepted writes to a
/// single row, at one version per write — so nothing a family can do gets near it. It
/// exists because [`seed_version`] lets a client choose a *new* row's starting number,
/// and a bound with no enforcement point is not a bound.
pub const MAX_SYNC_VERSION: i32 = 2_000_000_000;

/// The largest starting `version` a client may name for a row the server has never seen.
///
/// Seeds are accepted at all only because clients create rows offline and number them
/// locally before they can reach the server; the number is genuinely theirs at that
/// moment. But it must not be usable to place a row near the ceiling, because rows are
/// not always private: a `grocery_lists` row is shared with everyone the list was invited
/// to, so a seed of `MAX_SYNC_VERSION` would leave *the whole household* unable to edit
/// that list. A million is orders of magnitude past any real client's local counter and
/// leaves ~2000x headroom below the ceiling, so a hostile seed buys nothing.
pub const MAX_SEED_VERSION: i32 = 1_000_000;

/// The version a row takes after a write the server accepts.
///
/// Derived from the row already in the database and nothing else — the caller passes the
/// stored `version`, never the one on the wire.
///
/// `Err` is a [`AppError::Conflict`] (409, not 400): the request is well formed and the
/// caller did nothing wrong, the row itself simply cannot advance. 409 is also the status
/// clients already treat as "re-read and try again" rather than "this payload is broken".
pub fn advance_version(entity: &str, id: &str, server_version: i32) -> Result<i32, AppError> {
    if server_version >= MAX_SYNC_VERSION {
        return Err(AppError::Conflict(format!(
            "{} {} is at the maximum sync version ({}) and cannot be updated",
            entity, id, MAX_SYNC_VERSION
        )));
    }
    // `server_version` is now provably < MAX_SYNC_VERSION < i32::MAX, so this cannot
    // overflow. `saturating_add` all the same: the invariant lives in a database column
    // that a migration or a manual fix could in principle violate, and a silently wrong
    // number is worse than a stuck one.
    Ok(server_version.saturating_add(1))
}

/// The version a brand-new row starts at, taken from the client and bounded.
///
/// Refused with a 400 rather than clamped, so a client that is sending something absurd
/// is told which field is wrong instead of quietly having its numbering rewritten
/// underneath it. Negative seeds are refused for the same reason: `version` is a count,
/// and a negative one only ever comes from a bug or a probe.
pub fn seed_version(entity: &str, id: &str, client_version: i32) -> Result<i32, AppError> {
    if !(0..=MAX_SEED_VERSION).contains(&client_version) {
        return Err(AppError::BadRequest(format!(
            "{} {}: version must be between 0 and {} (got {})",
            entity, id, MAX_SEED_VERSION, client_version
        )));
    }
    Ok(client_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_version_advances_from_server_state() {
        assert_eq!(advance_version("drawing", "d1", 4).unwrap(), 5);
        assert_eq!(advance_version("drawing", "d1", 0).unwrap(), 1);
    }

    #[test]
    fn next_version_refuses_at_the_ceiling_instead_of_overflowing() {
        let err = advance_version("config", "c1", MAX_SYNC_VERSION).unwrap_err();
        assert!(matches!(err, AppError::Conflict(_)), "got {:?}", err);
        // One below the ceiling is still writable, so the bound is a wall and not a
        // fence that quietly starts a row apart from it.
        assert_eq!(
            advance_version("config", "c1", MAX_SYNC_VERSION - 1).unwrap(),
            MAX_SYNC_VERSION
        );
    }

    #[test]
    fn seed_version_accepts_ordinary_client_numbering() {
        assert_eq!(seed_version("drawing", "d1", 0).unwrap(), 0);
        assert_eq!(seed_version("drawing", "d1", 1).unwrap(), 1);
        assert_eq!(
            seed_version("drawing", "d1", MAX_SEED_VERSION).unwrap(),
            MAX_SEED_VERSION
        );
    }

    #[test]
    fn seed_version_refuses_an_inflated_or_negative_seed() {
        for bad in [MAX_SEED_VERSION + 1, MAX_SYNC_VERSION, i32::MAX, -1] {
            let err = seed_version("grocery list", "g1", bad).unwrap_err();
            assert!(matches!(err, AppError::BadRequest(_)), "got {:?} for {}", err, bad);
        }
    }
}
