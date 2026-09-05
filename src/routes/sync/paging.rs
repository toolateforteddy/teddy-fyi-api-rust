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
}
