//! How the sync processors turn one statement per item into one statement per run.
//!
//! # The problem
//!
//! Every table processor used to issue one `INSERT ... ON CONFLICT DO UPDATE` (or one
//! `UPDATE ... RETURNING`) per change. A sync body may legitimately carry 10,000 changes
//! for a single collection and 20,000 across all of them (`crate::routes::sync::limits`),
//! and the whole request runs inside one transaction holding one of the pool's sixteen
//! connections (`crate::db`). Twenty thousand round trips on one connection is the
//! dominant cost of a large sync, and none of it is work Postgres needs done separately:
//! the writes are uniform in shape over a fixed column set, so they collapse into
//! `INSERT ... SELECT ... FROM UNNEST($1::text[], $2::int4[], ...)`.
//!
//! # Why runs, and not one batch per operation kind
//!
//! The obvious shape — buffer every upsert, then every delete, and issue two statements —
//! is not equivalent to the loop it replaces, for two reasons, and both of them are
//! behaviours the sync path documents elsewhere and relies on:
//!
//! * **Order between kinds matters.** `crate::routes::sync::deletes` states that "within a
//!   single batch the ordering is already correct: changes are applied in the order they
//!   arrive, so an insert-then-delete pair in one request finds its row". Hoisting all
//!   upserts ahead of all deletes keeps that pair working but silently reverses the
//!   opposite one: a delete followed by a re-insert of the same id would end up deleted
//!   rather than present.
//! * **A repeated id inside one statement is an error, not a last-write-wins.**
//!   `INSERT ... ON CONFLICT DO UPDATE` refuses a command whose input names the same row
//!   twice ("ON CONFLICT DO UPDATE command cannot affect row a second time"), and the
//!   soft-delete `version = version + 1` is genuinely sequential — two deletes for one id
//!   move it two versions today, and a single set-based statement would move it one.
//!
//! So the processors buffer a *run*: the longest stretch of consecutive changes that share
//! one write kind and name distinct rows. A run is flushed as one statement, and the next
//! change starts a new one. The common payloads — a client uploading a screenful of edits,
//! or a first sync uploading everything it has — are a single run, so they get the full
//! collapse; a pathological alternating payload degrades to exactly the statement count it
//! has today and never to something incorrect. Equivalence is by construction: the flushed
//! statements are issued in the same order, and touch the same rows, as the loop did.
//!
//! [`RunTracker`] is only the bookkeeping for that rule. The column buffers and the
//! statements themselves stay in each processor, because they are what differs.

use std::collections::HashSet;

/// Decides where one run of batchable writes ends and the next begins.
///
/// `K` is a processor-local enum naming the write kinds that processor issues (a full
/// upsert, a version-only bump, a soft delete, ...). The tracker holds the kind of the run
/// being accumulated and the ids already in it; a change that disagrees with either has to
/// wait for the next statement.
pub struct RunTracker<K> {
    kind: Option<K>,
    ids: HashSet<String>,
}

impl<K: PartialEq> RunTracker<K> {
    pub fn new() -> Self {
        Self {
            kind: None,
            ids: HashSet::new(),
        }
    }

    /// Whether the buffered run must be issued before `(kind, id)` may join it.
    ///
    /// True when the run is for a different kind of write, or when it already contains
    /// `id` — see the module comment for why a repeat is a boundary rather than a
    /// de-duplication.
    pub fn needs_flush(&self, kind: &K, id: &str) -> bool {
        match &self.kind {
            None => false,
            Some(pending) => pending != kind || self.ids.contains(id),
        }
    }

    /// Whether the buffered run already holds a write for `id`, whatever its kind.
    ///
    /// `needs_flush` answers the same question for a write that is about to be buffered.
    /// This one exists for the *reads*: `config.rs` and `drawing.rs` cache their prefetched
    /// rows and mark an id stale once written, so the next lookup for it goes back to the
    /// database. That was exact while every write landed before the next read, and
    /// deferring writes into runs breaks it — the re-read would return the row as it was
    /// before the buffered write, and the second write would be numbered from a version
    /// that is already spent. So those two processors flush whenever this returns true,
    /// *before* consulting their cache, which puts the row in the database in time for the
    /// re-read to see it.
    pub fn contains(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// Notes that `(kind, id)` has been buffered into the current run.
    pub fn record(&mut self, kind: K, id: String) {
        self.kind = Some(kind);
        self.ids.insert(id);
    }

    /// Forgets the current run. Called immediately after the buffers it described are
    /// flushed.
    pub fn clear(&mut self) {
        self.kind = None;
        self.ids.clear();
    }
}

impl<K: PartialEq> Default for RunTracker<K> {
    fn default() -> Self {
        Self::new()
    }
}
