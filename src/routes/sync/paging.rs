//! Bounding one download, and telling the client where to pick the next one up.
//!
//! # Why a download needs a bound at all
//!
//! `validate_sync_payload` bounds what a client may *send*. Nothing bounded what the
//! server sends back: the download queries filter on `last_modified > cursor`, and an
//! initial sync (`last_synced_at` absent, or at or before the epoch) drops even the
//! echo-suppression filter, so the answer is every row the account owns with no `LIMIT`
//! anywhere. For `configs` that is a few hundred rows of a few kilobytes. For `drawings`
//! it is every drawing a family has ever made, each of them up to
//! `DEFAULT_MAX_DRAWING_DATA_BYTES`, materialised in memory and serialized into one
//! response — inside the transaction, on the first request every reinstalled client
//! makes. That is `context/2026-09-05_pre_split_changes.md` item 40.
//!
//! # Why the page boundary is a millisecond and not a row
//!
//! The cursor contract is already written down (see `crate::routes::sync::versioning`):
//! the client stores the `server_timestamp` it was handed and sends it back as
//! `last_synced_at`, and every download compares it with **strict** `>` against
//! `last_modified`, which is a millisecond stamped by the server. Paging has to extend
//! that idiom rather than invent a second one, because the cursor is the only thing the
//! client persists — there is no page token to hand back, and adding one would be a wire
//! break for every client that ships today.
//!
//! A millisecond cursor cannot address a position *inside* one millisecond. Every row a
//! single request writes shares that request's one clock reading, so a page that ends
//! halfway through a millisecond leaves the rest of that millisecond permanently
//! unreachable: the next sync asks for `> that millisecond` and skips them. So a page
//! must end on a whole millisecond. [`trim_page`] enforces exactly that, and reports the
//! one case where it cannot — more than a page of rows sharing a single millisecond,
//! which one oversized upload can produce — so the caller can serve that millisecond
//! whole instead of stalling the client in a loop that never advances.

use chrono::{DateTime, Utc};

/// What a probe page turned out to be.
///
/// The caller asks the database for `page_size + 1` rows ordered by `last_modified`; the
/// extra row is the probe, and its presence is what distinguishes "this is everything"
/// from "there is more behind it" without a second `COUNT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// Everything matching the filter fitted. The request's own `server_timestamp` is
    /// the client's next cursor, exactly as before pagination existed.
    Complete,
    /// The page was cut short. The client's next cursor is this millisecond, not the
    /// request's `server_timestamp`, or it would skip everything left behind.
    Truncated { next_cursor_ms: i64 },
    /// More than a page of rows carry one and the same `last_modified`, so there is no
    /// page boundary below it to stop at. The caller must re-read that millisecond in
    /// full and stop there; the group is bounded by what one request may write, which
    /// `DEFAULT_MAX_ITEMS_PER_COLLECTION` already caps.
    WholeMillisecond { ms: i64 },
}

/// The `LIMIT` one download read asks for.
///
/// `Some(n)` is a paged read: `n + 1` rows, the extra one being the probe [`trim_page`]
/// reads to tell "this is everything" from "there is more behind it". `None` is a client
/// that cannot resume a truncated download (see `SyncRequest::supports_paging`) and is
/// therefore served whole — expressed as a limit rather than as a second query, so the SQL
/// text and its prepared descriptor stay exactly the same on both paths.
pub fn probe_limit(page_size: Option<usize>) -> i64 {
    match page_size {
        // `usize` is 64-bit on every target this builds for, so the `min` is what keeps a
        // hand-set `SYNC_DOWNLOAD_PAGE_SIZE` near the top of the range from wrapping the
        // cast into a negative `LIMIT`.
        Some(n) => n.saturating_add(1).min(i64::MAX as usize) as i64,
        None => i64::MAX,
    }
}

/// The page size [`trim_page`] should trim to, given the same bound.
///
/// An unpaged read can never truncate, and `usize::MAX` says so in the one place that
/// decides: `rows.len() <= page_size` holds for every page a database can return, so
/// [`trim_page`] answers [`Page::Complete`] and no cursor is walked back.
pub fn trim_size(page_size: Option<usize>) -> usize {
    page_size.unwrap_or(usize::MAX)
}

/// Trims a probe page in place so that it ends on a whole millisecond.
///
/// `rows` must be ordered by `last_modified` ascending and must have been read with a
/// limit of `page_size + 1`. On return it holds at most `page_size` rows, and the trailing
/// partial millisecond — if any — has been dropped rather than delivered, because
/// delivering it would advance the client's cursor past rows it never received.
pub fn trim_page<T>(rows: &mut Vec<T>, page_size: usize, last_modified: impl Fn(&T) -> i64) -> Page {
    if rows.len() <= page_size {
        return Page::Complete;
    }

    // The probe row is the first row that did *not* fit, so its millisecond is the first
    // one this page cannot promise to have delivered in full.
    let boundary = last_modified(&rows[page_size]);
    rows.truncate(page_size);
    while rows.last().map(&last_modified).is_some_and(|ms| ms >= boundary) {
        rows.pop();
    }

    match rows.last().map(&last_modified) {
        Some(next_cursor_ms) => Page::Truncated { next_cursor_ms },
        None => Page::WholeMillisecond { ms: boundary },
    }
}

/// The `TIMESTAMPTZ` flavour of [`Page`].
///
/// `configs` and `drawings` store their cursor as a millisecond count; the todo and
/// grocery tables store theirs as `updated_at TIMESTAMPTZ`. The paging *rule* is the same
/// either way — a page must end on a whole instant, because the cursor comparison is a
/// strict `>` against a value many rows share — so this is the same [`trim_page`] with the
/// key read as microseconds and handed back as a timestamp.
///
/// Microseconds, not milliseconds: that is `timestamptz`'s own resolution, so the key is
/// exact and the cursor that comes back out is a value a row genuinely holds. Rounding to
/// milliseconds would hand back a cursor no row sits on, and a strict `>` against it would
/// re-deliver or skip depending on which side of the rounding the row fell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageAt {
    /// Everything matching the filter fitted.
    Complete,
    /// The page was cut short; this is the last instant it delivered whole.
    Truncated { next_cursor: DateTime<Utc> },
    /// More than a page of rows share this one instant, so it cannot be split. The caller
    /// serves that instant whole rather than stalling the client in a loop that never
    /// advances — see [`Page::WholeMillisecond`].
    WholeInstant { at: DateTime<Utc> },
}

/// Trims a probe page of `updated_at`-cursored rows so that it ends on a whole instant.
///
/// The `TIMESTAMPTZ` counterpart of [`trim_page`], with the same contract: `rows` must be
/// ordered by `updated_at` ascending and read with a limit of `page_size + 1`.
pub fn trim_page_at<T>(
    rows: &mut Vec<T>,
    page_size: usize,
    updated_at: impl Fn(&T) -> DateTime<Utc>,
) -> PageAt {
    // `timestamp_micros` saturates rather than wrapping, and the range it saturates at is
    // ~±294,000 years, so no value Postgres can hold in a `timestamptz` column collides
    // two distinct instants onto one key here.
    match trim_page(rows, page_size, |row| updated_at(row).timestamp_micros()) {
        Page::Complete => PageAt::Complete,
        Page::Truncated { next_cursor_ms: micros } => PageAt::Truncated {
            // Infallible in practice for the reason above; falling back to the row itself
            // keeps a hypothetical out-of-range value from silently becoming the epoch.
            next_cursor: DateTime::from_timestamp_micros(micros)
                .unwrap_or_else(|| updated_at(rows.last().expect("Truncated implies a kept row"))),
        },
        Page::WholeMillisecond { ms: micros } => PageAt::WholeInstant {
            at: DateTime::from_timestamp_micros(micros).unwrap_or_else(Utc::now),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(rows: &mut Vec<i64>, size: usize) -> Page {
        trim_page(rows, size, |ms| *ms)
    }

    #[test]
    fn a_page_that_fits_is_left_alone() {
        let mut rows = vec![1, 2, 3];
        assert_eq!(page(&mut rows, 3), Page::Complete);
        assert_eq!(rows, vec![1, 2, 3]);

        let mut rows = vec![1, 2];
        assert_eq!(page(&mut rows, 3), Page::Complete);
        assert_eq!(rows, vec![1, 2]);
    }

    #[test]
    fn one_row_over_the_page_truncates_and_reports_a_cursor() {
        let mut rows = vec![1, 2, 3, 4];
        assert_eq!(page(&mut rows, 3), Page::Truncated { next_cursor_ms: 3 });
        assert_eq!(rows, vec![1, 2, 3]);
    }

    #[test]
    fn a_millisecond_split_by_the_page_edge_is_dropped_whole() {
        // The page edge falls inside the group at 3, which a `>` cursor cannot address
        // halfway through, so the whole group waits for the next round.
        let mut rows = vec![1, 2, 3, 3, 3];
        assert_eq!(page(&mut rows, 3), Page::Truncated { next_cursor_ms: 2 });
        assert_eq!(rows, vec![1, 2]);
    }

    #[test]
    fn a_page_entirely_inside_one_millisecond_asks_for_the_whole_group() {
        let mut rows = vec![7, 7, 7, 7];
        assert_eq!(page(&mut rows, 3), Page::WholeMillisecond { ms: 7 });
    }

    #[test]
    fn an_unpaged_read_asks_for_everything_and_never_truncates() {
        assert_eq!(probe_limit(None), i64::MAX);

        // The bound a client that cannot resume is served under: whatever the database
        // returns fits, so the cursor is never walked back.
        let mut rows = vec![1, 2, 3, 3, 3];
        assert_eq!(page(&mut rows, trim_size(None)), Page::Complete);
        assert_eq!(rows, vec![1, 2, 3, 3, 3]);
    }

    #[test]
    fn a_paged_read_asks_for_one_row_past_the_page() {
        assert_eq!(probe_limit(Some(200)), 201);
        assert_eq!(trim_size(Some(200)), 200);
    }

    #[test]
    fn an_absurd_page_size_does_not_wrap_the_limit_negative() {
        // A `LIMIT` that came back negative would be a query error rather than a large
        // page, so the saturation matters more than the number it saturates to.
        assert!(probe_limit(Some(usize::MAX)) > 0);
    }
}
