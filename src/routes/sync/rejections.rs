//! What the server does with one item in a batch it refuses.
//!
//! # The model
//!
//! **A batch is all-or-nothing, and a refusal names the item that caused it.**
//!
//! Both halves are load-bearing, and until now only the first was actually true.
//!
//! [`crate::routes::sync::limits::validate_sync_payload`] already chose all-or-nothing
//! deliberately and wrote down the argument: a partially applied batch leaves the client
//! guessing which of its rows landed, whereas "an all-or-nothing 400 naming the offending
//! item is something it can act on". That reasoning holds exactly as far as the naming
//! does. A 400 that does *not* name the item is the worse of both worlds — the whole batch
//! is rejected and the client cannot tell which row to drop, so it resends the same batch
//! forever and that device stops syncing. Which is the same wedge
//! [`crate::routes::sync::deletes`] was written to remove, arriving by a different door.
//!
//! The authorization refusals were already fine: every `AppError::Forbidden` a processor
//! returns interpolates the id it is refusing ("User is not authorized to update grocery
//! item {id}"). The gap was the *payload* refusals. Ten processors deserialized an item,
//! and on failure returned `AppError::Serialization(err)` — a 400 whose body is serde's own
//! message, which names a field and a JSON offset ("missing field `name` at line 1 column
//! 50") for a payload the client sent as an array of hundreds. The id was written to the
//! server log and dropped from the response.
//!
//! [`item_payload_rejected`] is the one way to spell that refusal now, and
//! `tests::rejections` fails the build if a processor goes back to the bare error.
//!
//! # Why not per-item failure reporting
//!
//! The alternative — commit the good rows, hand the client a list of the rejected ones in
//! `upload_status` — is a bigger change than it looks, and it is deliberately not what this
//! module does. It is a wire-contract change on a response shape two client families parse,
//! and a client that does not learn to quarantine a rejected item retries it forever
//! regardless, so the wedge survives the fix. It also *is* the partial-commit question that
//! the split survey's item 27 is still holding open: committing part of a batch is exactly
//! what "three futures, three transactions" already does by accident, and deciding it here
//! by implementation would settle that question without anyone deciding it.
//!
//! So: all-or-nothing, named. If per-item reporting is ever wanted, it wants item 27
//! answered first and both clients in the room.

use crate::routes::sync::types::AppError;

/// The refusal for an item whose payload will not deserialize.
///
/// `entity` is the table's own name for the row ("todo item", "grocery list"), `id` is the
/// id the client sent, and `err` is serde's message, kept because it names the field.
/// Together they are the three things a client needs to drop exactly one row and retry:
/// what kind of thing, which one, and what was wrong with it.
///
/// [`AppError::BadRequest`] rather than [`AppError::Serialization`]: both answer 400, but
/// `Serialization`'s body is the serde error alone. This variant carries the whole sentence
/// through to the response, which is the entire point.
pub fn item_payload_rejected(entity: &str, id: &str, err: &serde_json::Error) -> AppError {
    AppError::BadRequest(format!("Invalid {entity} payload for {id}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serde_error() -> serde_json::Error {
        serde_json::from_str::<crate::routes::sync::types::TodoItemData>("{}")
            .expect_err("an empty object is not a todo item")
    }

    /// The three things the client is owed, in one string: what kind of row, which one, and
    /// what serde objected to.
    #[test]
    fn the_refusal_names_the_entity_the_id_and_the_reason() {
        let err = item_payload_rejected("todo item", "todo-42", &serde_error());

        match err {
            AppError::BadRequest(message) => {
                assert!(message.contains("todo item"), "{message}");
                assert!(message.contains("todo-42"), "{message}");
                assert!(message.contains("missing field"), "{message}");
            }
            other => panic!("expected BadRequest, got {other:?}"),
        }
    }

    /// `Serialization` answers 400 too, so the difference is not the status — it is that
    /// this variant's body reaches the client intact.
    #[test]
    fn the_refusal_is_a_bad_request_not_a_serialization_error() {
        assert!(matches!(
            item_payload_rejected("config", "config-1", &serde_error()),
            AppError::BadRequest(_)
        ));
    }
}
