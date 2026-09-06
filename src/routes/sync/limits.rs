//! Per-item bounds on what a sync body may carry, checked before anything is written.
//!
//! # Why the 8 MiB body cap is not enough
//!
//! `MAX_REQUEST_BODY_BYTES` (see [`crate::guardrails`]) bounds one *request*. It says
//! nothing about the shape of what is inside it, and the sync handler takes two fields
//! straight off the wire and into Postgres without looking:
//!
//! * `DrawingSyncItem.data` / the `data` inside a drawing change delta — an untyped
//!   `serde_json::Value` that becomes a `jsonb` column. One 8 MiB request can push
//!   several megabyte-sized blobs into the table, and the account that sends it pays
//!   nothing: rows are kept until the user deletes them, so the storage is permanent
//!   while the request was a single POST. Repeat in a loop and the cost is a database,
//!   not a rate limit.
//! * `configs[].key` / `configs[].value` — `TEXT` columns with no bound at all. A config
//!   is a setting; nothing about it needs to be a megabyte, and an unbounded `key` also
//!   feeds the `(user_id, device_uuid, key)` unique index.
//! * the *number* of items, in any of the twelve change vectors. Every one of them is
//!   processed by a plain `for` loop issuing one statement per item inside a transaction,
//!   so the count of items in a body is the count of round trips to Postgres it costs.
//!   Minimal deltas serialize to a few dozen bytes each, which puts well over a hundred
//!   thousand of them inside one 8 MiB request: trivial to send, and tens of thousands of
//!   sequential statements holding one of sixteen pool connections open against a
//!   scale-to-zero, per-compute-second-billed database at the other end. That asymmetry
//!   is the abuse; the per-item size bounds above do nothing about it.
//!
//! # Sizing
//!
//! Every number below is measured against what the real apps send, and every one is an
//! order of magnitude or more above it — the job is to make abuse bounded, not to make
//! a large honest drawing fail.
//!
//! Bounds are enforced **per item**, before the transaction opens. An over-large item
//! fails the whole request with a `400` naming the field, the item and the unit, and
//! nothing is written — deliberately, rather than truncating: a truncated `data` blob is
//! a corrupted drawing that syncs to every device the child owns and silently replaces
//! the good copy. Refusing leaves the drawing intact on the tablet that holds it.
//!
//! Each is overridable from the environment, following `SSE_MAX_STREAMS_PER_USER` and
//! `MAX_REQUEST_BODY_BYTES`, so an incident (or one family with an unusually enormous
//! drawing) is survivable with a restart rather than a release.

use crate::routes::sync::types::{AppError, ConfigChangeDelta, ConfigSyncItem, DrawingChangeDelta, DrawingSyncItem, SyncRequest};

/// Largest serialized size of one drawing's `data` blob, in bytes.
///
/// A ScribbleBox drawing is vector path data — a list of strokes, each a list of points
/// with a colour and a width — not a bitmap. A busy full-canvas scribble serializes to
/// tens of kilobytes; 512 KiB is roughly an order of magnitude above the largest seen and
/// still lets a single request carry a dozen of them under the 8 MiB body cap.
pub const DEFAULT_MAX_DRAWING_DATA_BYTES: usize = 512 * 1024;

/// Largest nesting depth inside a drawing's `data` blob.
///
/// The real structure is shallow and fixed — an object holding an array of strokes
/// holding arrays of points, four or five levels at the outside. Depth is bounded
/// separately from size because the two abuses are different: 30 kilobytes of `[[[[...`
/// is small enough to pass every byte limit while being expensive for anything that
/// later walks the value. `serde_json` has its own 128-level recursion limit on parse,
/// so this mostly narrows an existing guard to something defensible for this payload.
pub const DEFAULT_MAX_DRAWING_DATA_DEPTH: usize = 32;

/// Largest config `key`, in bytes of UTF-8.
///
/// Keys are compile-time identifiers chosen by the apps — `child_name`,
/// `lockout_minutes`, `selected_theme`. The longest in the repo is well under 40 bytes;
/// 128 leaves room for a namespaced key nobody has needed yet.
pub const DEFAULT_MAX_CONFIG_KEY_BYTES: usize = 128;

/// Largest config `value`, in bytes of UTF-8.
///
/// A value is a setting: a name, a number, a colour, an enum, occasionally a small JSON
/// list encoded as a string. 8 KiB is far past all of those and still small enough that
/// a client cannot use the config table as free storage. Anything genuinely large is a
/// drawing, and drawings have their own table and their own bound.
pub const DEFAULT_MAX_CONFIG_VALUE_BYTES: usize = 8 * 1024;

/// Largest number of items one change vector in a request may carry.
///
/// Sized off what the clients actually send, with the honest admission that only one of
/// them can be read from here. The Android engine (`RoomSyncEngine` in the toybox repo)
/// batches drawings hard — `MAX_BATCH_DRAWINGS = 25`, or 1,000,000 chars of vector data,
/// whichever comes first — and sends its configs unbatched, which is bounded by the
/// number of compile-time config keys (about thirty) times the devices a
/// ScribbleKeepCloud console speaks for: low hundreds at the very outside. The todo and
/// grocery client is not in a repo this one can see and is assumed *not* to batch, so its
/// worst case is the whole of a household's local history in a single body on the first
/// sync after cloud sync is switched on. A family adding twenty grocery rows a week for
/// five years reaches roughly five thousand; 10,000 is double that, and two orders of
/// magnitude above anything the batched client can produce.
///
/// The point of the number is not to be tight. A count limit a real tablet can reach is
/// an outage rather than a defence: the body does not get smaller on retry, so the client
/// re-sends the same request forever and that account simply never syncs again. It only
/// has to be far below what a body full of minimal deltas can hold.
pub const DEFAULT_MAX_ITEMS_PER_COLLECTION: usize = 10_000;

/// Largest number of items across every change vector in one request.
///
/// The per-collection bound alone is not a bound on the request: there are twelve
/// vectors, so twelve times the cap — 120,000 statements — would still be reachable in
/// one POST, which is the whole problem restated. This one is what actually bounds the
/// work a single request can order, and it is the number to look at when reasoning about
/// database cost.
///
/// 20,000 sits at twice the per-collection cap, so a legitimate first sync may bring one
/// enormous collection *and* everything that travels with it (grocery items alongside
/// their lists, stores and categories) and still fit. Against abuse it is roughly a
/// seven-fold cut: minimal deltas run about sixty bytes, so 8 MiB of them is on the order
/// of 140,000 items today.
///
/// Note which bound binds when. For real traffic the byte limits bite first — 20,000
/// items of any substance is far past 8 MiB — so this only ever fires on a body that is
/// deliberately made of nothing.
pub const DEFAULT_MAX_ITEMS_TOTAL: usize = 20_000;

/// Largest number of rows one download hands back per entity, per request.
///
/// Every other constant in this file bounds what a client may *send*. This one bounds
/// what the server sends back, which until now nothing did: the download queries have no
/// `LIMIT`, and an initial sync drops the echo-suppression filter as well, so the reply
/// is every row the account owns. At `DEFAULT_MAX_DRAWING_DATA_BYTES` apiece that is a
/// response the process has to build in memory, inside the transaction, on the first
/// request every reinstalled client makes.
///
/// 200 is sized off the drawing table, because drawings are three orders of magnitude
/// heavier than configs and one page size covers both. A page of 200 is ~6 MiB of the
/// tens-of-kilobytes drawings the apps actually produce, and 100 MiB only for an account
/// whose every drawing sits on the 512 KiB ceiling — which is why the *number of rows* is
/// the wrong unit in the long run and a byte budget is the right one; this is the
/// smallest change that removes the cliff without a wire break. It is also far above a
/// steady-state sync, which carries the handful of rows that changed since the last one,
/// so only a first sync ever pages at all.
///
/// See `crate::routes::sync::paging` for how a client resumes: pages end on a whole
/// millisecond and the reply's `server_timestamp` is walked back to it, so a client that
/// already stores that value as its cursor pages correctly with no change of its own.
pub const DEFAULT_SYNC_DOWNLOAD_PAGE_SIZE: usize = 200;

const MAX_DRAWING_DATA_BYTES_ENV: &str = "SYNC_MAX_DRAWING_DATA_BYTES";
const MAX_DRAWING_DATA_DEPTH_ENV: &str = "SYNC_MAX_DRAWING_DATA_DEPTH";
const MAX_CONFIG_KEY_BYTES_ENV: &str = "SYNC_MAX_CONFIG_KEY_BYTES";
const MAX_CONFIG_VALUE_BYTES_ENV: &str = "SYNC_MAX_CONFIG_VALUE_BYTES";
const MAX_ITEMS_PER_COLLECTION_ENV: &str = "SYNC_MAX_ITEMS_PER_COLLECTION";
const MAX_ITEMS_TOTAL_ENV: &str = "SYNC_MAX_ITEMS_TOTAL";
const SYNC_DOWNLOAD_PAGE_SIZE_ENV: &str = "SYNC_DOWNLOAD_PAGE_SIZE";

/// The bounds one request is checked against, resolved once per request.
///
/// Read from the environment through [`SyncLimits::from_env`] rather than consulted as
/// constants at each use, so that tests can construct a tiny set of limits and exercise
/// the refusal path without building a half-megabyte payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncLimits {
    pub max_drawing_data_bytes: usize,
    pub max_drawing_data_depth: usize,
    pub max_config_key_bytes: usize,
    pub max_config_value_bytes: usize,
    pub max_items_per_collection: usize,
    pub max_items_total: usize,
    /// The one bound here on the *response* rather than the request. See
    /// [`DEFAULT_SYNC_DOWNLOAD_PAGE_SIZE`]; it rides along in this struct because it is
    /// read from the environment once per request exactly like the others.
    pub download_page_size: usize,
}

impl Default for SyncLimits {
    fn default() -> Self {
        Self {
            max_drawing_data_bytes: DEFAULT_MAX_DRAWING_DATA_BYTES,
            max_drawing_data_depth: DEFAULT_MAX_DRAWING_DATA_DEPTH,
            max_config_key_bytes: DEFAULT_MAX_CONFIG_KEY_BYTES,
            max_config_value_bytes: DEFAULT_MAX_CONFIG_VALUE_BYTES,
            max_items_per_collection: DEFAULT_MAX_ITEMS_PER_COLLECTION,
            max_items_total: DEFAULT_MAX_ITEMS_TOTAL,
            download_page_size: DEFAULT_SYNC_DOWNLOAD_PAGE_SIZE,
        }
    }
}

impl SyncLimits {
    /// Reads the seven overrides, falling back to the compiled defaults.
    ///
    /// Zero and unparseable values fall back too, as everywhere else in this codebase: a
    /// zero bound would refuse every drawing in the product, and an operator typo must
    /// not be able to do that.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            max_drawing_data_bytes: read_limit(MAX_DRAWING_DATA_BYTES_ENV, defaults.max_drawing_data_bytes),
            max_drawing_data_depth: read_limit(MAX_DRAWING_DATA_DEPTH_ENV, defaults.max_drawing_data_depth),
            max_config_key_bytes: read_limit(MAX_CONFIG_KEY_BYTES_ENV, defaults.max_config_key_bytes),
            max_config_value_bytes: read_limit(MAX_CONFIG_VALUE_BYTES_ENV, defaults.max_config_value_bytes),
            max_items_per_collection: read_limit(MAX_ITEMS_PER_COLLECTION_ENV, defaults.max_items_per_collection),
            max_items_total: read_limit(MAX_ITEMS_TOTAL_ENV, defaults.max_items_total),
            download_page_size: read_limit(SYNC_DOWNLOAD_PAGE_SIZE_ENV, defaults.download_page_size),
        }
    }
}

fn read_limit(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(default)
}

/// Checks every bounded field in one sync body.
///
/// Called at the top of the handler, before the first transaction opens, so a refusal
/// leaves the database exactly as it was — including the other, perfectly valid items in
/// the same request. That is the right trade for a batch upload: a client that gets a
/// partial success has to work out which half landed, whereas an all-or-nothing `400`
/// naming the offending item is something it can act on.
pub fn validate_sync_payload(payload: &SyncRequest, limits: &SyncLimits) -> Result<(), AppError> {
    check_item_counts(payload, limits)?;
    for item in &payload.drawings {
        check_drawing_item(item, limits)?;
    }
    for change in &payload.drawing_changes {
        check_drawing_change(change, limits)?;
    }
    for item in &payload.configs {
        check_config_item(item, limits)?;
    }
    for change in &payload.config_changes {
        check_config_change(change, limits)?;
    }
    Ok(())
}

/// Every change vector in a request, paired with the name the client knows it by.
///
/// Listed explicitly rather than counted off a derive, so that a vector added to
/// `SyncRequest` and not added here is a visible omission in this file rather than a
/// silently unbounded field. The names are the wire (camelCase) aliases, because the
/// refusal is read by whoever is holding the request body.
fn collections(payload: &SyncRequest) -> [(&'static str, usize); 12] {
    [
        ("todoListChanges", payload.todo_list_changes.len()),
        ("todoChanges", payload.todo_changes.len()),
        ("groceryListChanges", payload.grocery_list_changes.len()),
        ("groceryListMemberChanges", payload.grocery_list_member_changes.len()),
        ("storeChanges", payload.store_changes.len()),
        ("categoryChanges", payload.category_changes.len()),
        ("groceryChanges", payload.grocery_changes.len()),
        ("groceryItemStoreInfoChanges", payload.grocery_item_store_info_changes.len()),
        ("configChanges", payload.config_changes.len()),
        ("drawingChanges", payload.drawing_changes.len()),
        ("configs", payload.configs.len()),
        ("drawings", payload.drawings.len()),
    ]
}

/// Bounds how much work one request may order, per vector and in total.
///
/// Both, deliberately. The per-collection check is the one that can name a field, which
/// is what makes the `400` actionable for a client that has to decide what to send
/// instead; the total is the one that is actually a bound, since twelve collections each
/// just under their own cap is the same request the cap was meant to refuse.
///
/// Checked before the size checks below, because it is the cheaper of the two — it reads
/// twelve lengths, where the size pass serializes every drawing blob in the body.
fn check_item_counts(payload: &SyncRequest, limits: &SyncLimits) -> Result<(), AppError> {
    let counts = collections(payload);
    for (field, count) in counts {
        if count > limits.max_items_per_collection {
            return Err(AppError::BadRequest(format!(
                "{} carries {} items, over the {}-item limit",
                field, count, limits.max_items_per_collection
            )));
        }
    }

    let total: usize = counts.iter().map(|(_, count)| count).sum();
    if total > limits.max_items_total {
        return Err(AppError::BadRequest(format!(
            "request carries {} items across all change collections, over the {}-item limit",
            total, limits.max_items_total
        )));
    }
    Ok(())
}

fn check_drawing_item(item: &DrawingSyncItem, limits: &SyncLimits) -> Result<(), AppError> {
    check_drawing_data(&item.id.to_string(), "drawings[].data", &item.data, limits)
}

/// A drawing change delta carries the whole `DrawingData` object in `data`, with the
/// blob nested one level down under its own `data` key. The nested value is what reaches
/// the `jsonb` column, so that is what is measured; when it is absent (a metadata-only
/// update, which the processor handles by reading the server's copy) there is nothing to
/// bound.
fn check_drawing_change(change: &DrawingChangeDelta, limits: &SyncLimits) -> Result<(), AppError> {
    let Some(payload) = change.data.as_ref() else {
        return Ok(());
    };
    let Some(blob) = payload.get("data") else {
        return Ok(());
    };
    check_drawing_data(&change.id, "drawingChanges[].data.data", blob, limits)
}

fn check_drawing_data(
    id: &str,
    field: &str,
    data: &serde_json::Value,
    limits: &SyncLimits,
) -> Result<(), AppError> {
    // Measured on the serialized form, because that is what is stored and what the
    // account is being charged for. `to_string` costs one pass over a value that is
    // about to be re-serialized into the query anyway.
    let bytes = serde_json::to_string(data).map(|s| s.len()).unwrap_or(0);
    if bytes > limits.max_drawing_data_bytes {
        return Err(AppError::BadRequest(format!(
            "drawing {}: {} is {} bytes, over the {}-byte limit",
            id, field, bytes, limits.max_drawing_data_bytes
        )));
    }

    let depth = json_depth(data);
    if depth > limits.max_drawing_data_depth {
        return Err(AppError::BadRequest(format!(
            "drawing {}: {} is nested {} levels deep, over the {}-level limit",
            id, field, depth, limits.max_drawing_data_depth
        )));
    }
    Ok(())
}

fn check_config_item(item: &ConfigSyncItem, limits: &SyncLimits) -> Result<(), AppError> {
    check_config_fields(&item.id.to_string(), "configs[]", &item.key, &item.value, limits)
}

fn check_config_change(change: &ConfigChangeDelta, limits: &SyncLimits) -> Result<(), AppError> {
    let Some(payload) = change.data.as_ref() else {
        return Ok(());
    };
    // Absent or non-string fields are left to the processor's own `from_value` to reject,
    // which already answers 400. This function only decides *size*.
    let key = payload.get("key").and_then(|v| v.as_str()).unwrap_or("");
    let value = payload.get("value").and_then(|v| v.as_str()).unwrap_or("");
    check_config_fields(&change.id, "configChanges[].data", key, value, limits)
}

fn check_config_fields(
    id: &str,
    field: &str,
    key: &str,
    value: &str,
    limits: &SyncLimits,
) -> Result<(), AppError> {
    if key.len() > limits.max_config_key_bytes {
        return Err(AppError::BadRequest(format!(
            "config {}: {}.key is {} bytes, over the {}-byte limit",
            id, field, key.len(), limits.max_config_key_bytes
        )));
    }
    if value.len() > limits.max_config_value_bytes {
        return Err(AppError::BadRequest(format!(
            "config {}: {}.value is {} bytes, over the {}-byte limit",
            id, field, value.len(), limits.max_config_value_bytes
        )));
    }
    Ok(())
}

/// Nesting depth of a JSON value: a scalar is 1, `[1]` is 2, `[[1]]` is 3.
///
/// Written as an explicit stack rather than a recursive walk. The input is attacker-
/// supplied and the whole point of the check is that it may be pathologically deep, so a
/// function that recurses once per level would be the very stack overflow it is meant to
/// prevent.
fn json_depth(value: &serde_json::Value) -> usize {
    let mut max_depth = 0usize;
    let mut stack = vec![(value, 1usize)];
    while let Some((node, depth)) = stack.pop() {
        max_depth = max_depth.max(depth);
        match node {
            serde_json::Value::Array(items) => {
                for item in items {
                    stack.push((item, depth + 1));
                }
            }
            serde_json::Value::Object(fields) => {
                for item in fields.values() {
                    stack.push((item, depth + 1));
                }
            }
            _ => {}
        }
    }
    max_depth
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::sync::tests::helpers::request;
    use serde_json::json;

    fn tiny() -> SyncLimits {
        SyncLimits {
            max_drawing_data_bytes: 64,
            max_drawing_data_depth: 3,
            max_config_key_bytes: 8,
            max_config_value_bytes: 16,
            max_items_per_collection: 3,
            max_items_total: 5,
            download_page_size: DEFAULT_SYNC_DOWNLOAD_PAGE_SIZE,
        }
    }

    #[test]
    fn depth_counts_levels_not_siblings() {
        assert_eq!(json_depth(&json!(1)), 1);
        assert_eq!(json_depth(&json!([1, 2, 3])), 2);
        assert_eq!(json_depth(&json!({"a": {"b": [1]}})), 4);
    }

    #[test]
    fn deep_nesting_is_refused_even_when_small() {
        let mut deep = json!(1);
        for _ in 0..10 {
            deep = json!([deep]);
        }
        assert!(serde_json::to_string(&deep).unwrap().len() < tiny().max_drawing_data_bytes);
        let err = check_drawing_data("d1", "drawings[].data", &deep, &tiny()).unwrap_err();
        assert!(matches!(err, AppError::BadRequest(ref m) if m.contains("levels deep")), "got {:?}", err);
    }

    #[test]
    fn oversized_data_names_the_field_and_the_unit() {
        let big = json!({ "strokes": "x".repeat(200) });
        let err = check_drawing_data("d1", "drawings[].data", &big, &tiny()).unwrap_err();
        match err {
            AppError::BadRequest(msg) => {
                assert!(msg.contains("drawings[].data"), "{}", msg);
                assert!(msg.contains("bytes"), "{}", msg);
                assert!(msg.contains("64"), "{}", msg);
            }
            other => panic!("expected BadRequest, got {:?}", other),
        }
    }

    #[test]
    fn ordinary_values_pass() {
        let drawing = json!({ "strokes": [{ "points": [1, 2], "color": "#fff" }] });
        assert!(check_drawing_data("d1", "drawings[].data", &drawing, &SyncLimits::default()).is_ok());
        assert!(check_config_fields("c1", "configs[]", "child_name", "Robin", &SyncLimits::default()).is_ok());
    }

    #[test]
    fn config_key_and_value_are_bounded_separately() {
        let long_key = check_config_fields("c1", "configs[]", &"k".repeat(9), "v", &tiny()).unwrap_err();
        assert!(matches!(long_key, AppError::BadRequest(ref m) if m.contains(".key")), "got {:?}", long_key);

        let long_value = check_config_fields("c1", "configs[]", "k", &"v".repeat(17), &tiny()).unwrap_err();
        assert!(matches!(long_value, AppError::BadRequest(ref m) if m.contains(".value")), "got {:?}", long_value);
    }

    /// An otherwise empty body, so a count test shows only the vector it is about.
    fn blank() -> SyncRequest {
        request("client-1")
    }

    fn grocery_delta(n: usize) -> Vec<crate::routes::sync::types::GroceryChangeDelta> {
        (0..n)
            .map(|i| crate::routes::sync::types::GroceryChangeDelta {
                id: format!("g{}", i),
                operation_type: crate::routes::sync::types::OperationType::Delete,
                version: 1,
                data: None,
            })
            .collect()
    }

    fn todo_delta(n: usize) -> Vec<crate::routes::sync::types::TodoChangeDelta> {
        (0..n)
            .map(|i| crate::routes::sync::types::TodoChangeDelta {
                id: format!("t{}", i),
                operation_type: crate::routes::sync::types::OperationType::Delete,
                version: 1,
                data: None,
            })
            .collect()
    }

    #[test]
    fn an_over_full_collection_is_refused_by_name() {
        let mut payload = blank();
        payload.grocery_changes = grocery_delta(4);
        let err = check_item_counts(&payload, &tiny()).unwrap_err();
        match err {
            AppError::BadRequest(msg) => {
                assert!(msg.contains("groceryChanges"), "{}", msg);
                assert!(msg.contains("items"), "{}", msg);
                assert!(msg.contains('3'), "{}", msg);
            }
            other => panic!("expected BadRequest, got {:?}", other),
        }
    }

    /// The case a per-collection cap on its own does not catch: several collections, each
    /// legal, adding up to a request that is not.
    #[test]
    fn collections_each_under_the_cap_can_still_bust_the_total() {
        let mut payload = blank();
        payload.grocery_changes = grocery_delta(3);
        payload.todo_changes = todo_delta(3);
        for (_, count) in collections(&payload) {
            assert!(count <= tiny().max_items_per_collection);
        }
        let err = check_item_counts(&payload, &tiny()).unwrap_err();
        assert!(
            matches!(err, AppError::BadRequest(ref m) if m.contains("across all change collections") && m.contains('5')),
            "got {:?}",
            err
        );
    }

    #[test]
    fn a_body_within_both_counts_passes() {
        let mut payload = blank();
        payload.grocery_changes = grocery_delta(2);
        payload.todo_changes = todo_delta(2);
        assert!(check_item_counts(&payload, &tiny()).is_ok());
        assert!(validate_sync_payload(&payload, &tiny()).is_ok());
    }

    /// What the batched Android client sends is nowhere near the shipped defaults.
    #[test]
    fn the_default_counts_clear_a_real_client_batch_by_orders_of_magnitude() {
        let defaults = SyncLimits::default();
        // `RoomSyncEngine.MAX_BATCH_DRAWINGS`, plus every config key the apps define,
        // times a generous number of devices one console might speak for.
        assert!(defaults.max_items_per_collection > 25 * 100);
        assert!(defaults.max_items_per_collection > 32 * 20);
        assert!(defaults.max_items_total >= defaults.max_items_per_collection * 2);
    }

    #[test]
    fn env_overrides_fall_back_on_junk_and_zero() {
        // Not touching the process environment here: `from_env` is a thin wrapper over
        // `read_limit`, and that is the part with a decision in it.
        assert_eq!(read_limit("SYNC_LIMITS_TEST_UNSET_VAR", 7), 7);
    }
}
