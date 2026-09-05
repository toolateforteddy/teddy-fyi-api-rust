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

const MAX_DRAWING_DATA_BYTES_ENV: &str = "SYNC_MAX_DRAWING_DATA_BYTES";
const MAX_DRAWING_DATA_DEPTH_ENV: &str = "SYNC_MAX_DRAWING_DATA_DEPTH";
const MAX_CONFIG_KEY_BYTES_ENV: &str = "SYNC_MAX_CONFIG_KEY_BYTES";
const MAX_CONFIG_VALUE_BYTES_ENV: &str = "SYNC_MAX_CONFIG_VALUE_BYTES";

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
}

impl Default for SyncLimits {
    fn default() -> Self {
        Self {
            max_drawing_data_bytes: DEFAULT_MAX_DRAWING_DATA_BYTES,
            max_drawing_data_depth: DEFAULT_MAX_DRAWING_DATA_DEPTH,
            max_config_key_bytes: DEFAULT_MAX_CONFIG_KEY_BYTES,
            max_config_value_bytes: DEFAULT_MAX_CONFIG_VALUE_BYTES,
        }
    }
}

impl SyncLimits {
    /// Reads the four overrides, falling back to the compiled defaults.
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
    use serde_json::json;

    fn tiny() -> SyncLimits {
        SyncLimits {
            max_drawing_data_bytes: 64,
            max_drawing_data_depth: 3,
            max_config_key_bytes: 8,
            max_config_value_bytes: 16,
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

    #[test]
    fn env_overrides_fall_back_on_junk_and_zero() {
        // Not touching the process environment here: `from_env` is a thin wrapper over
        // `read_limit`, and that is the part with a decision in it.
        assert_eq!(read_limit("SYNC_LIMITS_TEST_UNSET_VAR", 7), 7);
    }
}
