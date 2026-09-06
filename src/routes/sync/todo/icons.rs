//! Server-assigned todo icons, resolved *before* the sync transaction opens.
//!
//! # Why this is not done inside `process_todo_changes`
//!
//! Assigning an icon is an outbound HTTPS round trip to Gemini: hundreds of
//! milliseconds on a good day, and bounded only by the request timeout on a bad
//! one. It used to happen in the middle of the todo transaction, which meant one
//! of the pool's 16 connections (`crate::db`) was held open, inside a live
//! Postgres transaction, for the entire duration of somebody else's API call.
//! A handful of concurrent syncs was enough to hold every connection in that
//! state and make *unrelated* endpoints fail on `acquire_timeout`.
//!
//! Nothing about the decision needs the database: the predicate reads the title
//! and icon out of the request payload, and the budget lives in Redis. So the
//! calls are made here, up front, and the transaction is handed a finished
//! `id -> icon` map and does nothing but database work.
//!
//! The candidates are resolved concurrently. There are at most two of them (see
//! the batch-size gate below), they no longer hold a transaction while they run,
//! and two sequential Gemini round trips cost a client twice the latency for no
//! reason.

use crate::routes::ai::budget::{charge_gemini_call, BudgetLimits};
use crate::routes::ai::service::assign_todo_icon;
use crate::routes::sync::types::{OperationType, TodoChangeDelta, TodoItemData};
use crate::state::AppState;
use std::collections::HashMap;

/// True when this change is one the server would like to put an icon on.
///
/// Kept as its own function because the same test is applied twice: once here,
/// to decide what to spend a Gemini call on, and once at the write site in
/// `super::todo_items`, to decide whether to actually take the resolved icon.
/// The write site re-checks rather than trusting the map so that "when an icon
/// is assigned" has exactly one definition and stays where the write happens.
pub(super) fn wants_server_icon(item: &TodoItemData, batch_len: usize) -> bool {
    // Only for small batches: a device doing a first-run bulk upload is not
    // asking for forty icons, and this is the path that spends money.
    batch_len < 3
        && item.icon.as_deref().unwrap_or("").is_empty()
        && !item.title.trim().is_empty()
}

/// Resolves the icons this batch of todo changes wants, before any transaction
/// is open.
///
/// Returns `change id -> icon`. Absence means "no icon for this one", for every
/// reason indistinguishably: the change did not qualify, the account was out of
/// AI budget, or Gemini failed. That flattening is deliberate — see the failure
/// note below.
pub async fn resolve_todo_icons(
    state: &AppState,
    user_id: &str,
    changes: &[TodoChangeDelta],
) -> HashMap<String, String> {
    // Cheap bail-out that also keeps the deserialization below off the hot path
    // for the bulk uploads that can never qualify anyway.
    if changes.len() >= 3 {
        return HashMap::new();
    }

    // A deployment with no `GEMINI_API_KEY` has no icons to offer. The two AI endpoints
    // answer 503 in this case (`crate::routes::ai::require_gemini_api_key`); this path
    // must not, because it runs inside somebody's sync and an icon is a garnish -- the
    // same reasoning that swallows a budget refusal below. Checked here rather than in
    // the closure so an unconfigured deployment does not charge the budget for calls it
    // was never going to make.
    let Some(gemini_api_key) = state.gemini_api_key.as_deref() else {
        return HashMap::new();
    };

    let candidates: Vec<(&str, String)> = changes
        .iter()
        .filter(|change| {
            matches!(
                change.operation_type,
                OperationType::Insert | OperationType::Update
            )
        })
        .filter_map(|change| {
            // A payload that will not deserialize is not this function's problem;
            // `process_todo_changes` is where that becomes the error the client
            // sees. Here it simply means "no icon".
            let data = change.data.as_ref()?;
            let item: TodoItemData = serde_json::from_value(data.clone()).ok()?;
            wants_server_icon(&item, changes.len()).then_some((change.id.as_str(), item.title))
        })
        .collect();

    if candidates.is_empty() {
        return HashMap::new();
    }

    let resolved = futures_util::future::join_all(candidates.into_iter().map(|(id, title)| async move {
        // This is the third path that spends the Gemini budget, and the least
        // obvious one — a client that never calls `/api/assign-icon` can still
        // bill us by syncing icon-less todos in batches of one or two. It
        // therefore charges the same per-account budget as the explicit
        // endpoints.
        //
        // A refusal is swallowed rather than propagated: the icon is a garnish,
        // and failing somebody's whole sync because their AI allowance ran out
        // would turn a spend limit into data loss. The same goes for an error
        // from Gemini itself. Both simply leave the id out of the map, and the
        // todo is written with the icon the client sent (usually none).
        charge_gemini_call(&state.redis_client, BudgetLimits::cached(), user_id)
            .await
            .ok()?;
        let icon = assign_todo_icon(&state.http_client, gemini_api_key, &title)
            .await
            .ok()?;
        Some((id.to_string(), icon))
    }))
    .await;

    resolved.into_iter().flatten().collect()
}
