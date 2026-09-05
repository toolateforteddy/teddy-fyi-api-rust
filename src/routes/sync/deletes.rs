//! What the server does with a delete for a row it has never seen.
//!
//! # The policy
//!
//! **A delete names an intent, not a row. If the row is not there, the intent is
//! already satisfied, so the delete succeeds and is acknowledged.**
//!
//! This is the only sanctioned shape for a soft-delete in the sync path. It used to
//! be written through a `soft_delete_version!` macro that owned the `fetch_optional`,
//! so that a processor could not spell the delete in a way that turned a missing row
//! into an error. The processors now delete a whole run of ids in one statement
//! (`crate::routes::sync::batching`), which is a different shape -- `WHERE id = ANY($n)
//! ... RETURNING id, version`, `fetch_all`, and the ids that come back are the rows
//! that were there -- so the macro is gone and [`ack_unsynced_delete`] is what the
//! processors share. The rule it enforced is unchanged, and the guard that keeps it
//! honest is still `tests::deletes`, which fails the build for a soft-delete run
//! through `fetch_one`.
//!
//! # Why "acknowledge", rather than 500 or 403
//!
//! Five of the seven table processors used to run the soft-delete `UPDATE ...
//! RETURNING version` through `fetch_one` *outside* the `if let Some(row) =
//! record` guard that had just looked the row up. On a row the server does not
//! have, `fetch_one` returns `RowNotFound`, `?` turns it into
//! [`AppError::Database`], and the caller gets a 500 — with the whole batch, todo
//! and grocery and scribble alike, rolled back, because every processor shares one
//! transaction. Two more answered a missing row with a 403 ("grocery list not
//! found", "parent grocery item not found"), which fails the same batch just as
//! hard.
//!
//! Getting there needs no hostile client, only an ordinary one:
//!
//! * A row created offline and deleted before it ever synced. The client is
//!   *expected* to drop those locally instead of sending them, and today it does —
//!   there is one `if` on the client that checks whether the row was ever synced.
//!   The whole failure mode is one wrong `if` away, on a client the server does not
//!   ship and cannot fix in a hurry.
//! * A row a previous account deletion hard-removed, still pending on a device that
//!   was offline when it happened.
//! * A retried batch whose first attempt landed, if a table ever hard-deletes.
//!
//! And the failure does not stop at one request. The response says only "500"; it
//! does not name the row that caused it, so the client cannot drop the offending
//! change even in principle. It resends the same batch, gets the same 500, and that
//! device stops syncing entirely — every drawing, list and todo on it stuck behind
//! one tombstone for a row nobody has.
//!
//! Acknowledging costs nothing in return. There is no row to protect: the id names
//! nothing, so there is nothing to leak and no one to wrong. Authorization is
//! unaffected — a delete for a row that *does* exist and is not the caller's is
//! still a 403, decided before this module is reached.
//!
//! # Why no tombstone row is written
//!
//! The acknowledgement leaves the table untouched rather than inserting a
//! `is_deleted = TRUE` placeholder. A placeholder would need every NOT NULL column
//! the table has (`title`, `"userId"`, `"createdAt"` ...) invented from a payload
//! that carries none of them, and it would be a row the account did not ask for.
//!
//! The price is that an insert arriving *after* a delete for the same id
//! resurrects it. That ordering does not happen for the case this module is about
//! — the server never saw the row, so no other device can be holding one to insert
//! — and it needs a client that both keeps a pending insert and a pending delete
//! for one id and uploads them in that order. Within a single batch the ordering is
//! already correct: changes are applied in the order they arrive, so an
//! insert-then-delete pair in one request finds its row.

/// The `version` reported back for a delete of a row the server never had.
///
/// The number is a formality — the client is told the change is `SYNCED` so it can
/// drop it, and there is no row for the version to describe. `1` is what
/// `grocery_item_store_info` already reported for this case, and it is what a row
/// seeded by [`seed_version`](crate::routes::sync::versioning::seed_version) and
/// deleted at once would have carried, so nothing downstream sees a version it
/// would not otherwise see.
pub const UNSYNCED_DELETE_VERSION: i32 = 1;

/// Acknowledges a delete for a row that is not in the table, and returns the
/// `version` to report for it.
///
/// Logged at `info` with the id only — an id the server has no row for is not user
/// content, and this is the one trace that a device is sending deletes for rows
/// that were never uploaded, which is worth being able to see if a client
/// regresses. See `tests::log_hygiene` for what the sync path may not log.
pub fn ack_unsynced_delete(entity: &str, id: &str) -> i32 {
    tracing::info!(
        "Delete for {} {} matched no row; acknowledging it as already deleted",
        entity,
        id
    );
    UNSYNCED_DELETE_VERSION
}
