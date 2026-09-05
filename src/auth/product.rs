//! Which of the two products a credential belongs to.
//!
//! One service currently serves two of them — teddy.fyi (the household grocery and todo
//! apps) and ScribbleRoute (ScribbleKeep / ScribbleBox on Fire tablets) — and until the
//! backend split there is nothing on the boundary between them: a token minted for one is
//! structurally valid for the other, because the only thing an access token asserts is a
//! signature, an `exp` and a device. `JWT_SECRET` is deliberately *not* rotated at the
//! cutover, so from the traffic flip until decommissioning that stays true across two
//! hostnames unless the token itself says which product it came from.
//!
//! This enum is that claim. It is derived from the audience of the Google ID token the
//! session was established with (see [`crate::auth::client_ids`]), carried in
//! [`crate::auth::tokens::Claims`], persisted on the `sessions` row so a refresh does not
//! lose it, and enforced against the requested `SyncScope`.
//!
//! # Why `Option<Product>` appears everywhere
//!
//! Access tokens live fifteen minutes but the sessions behind them live seven days, and
//! the two binaries this repository is about to become cannot deploy at the same instant.
//! So the rollout has three stages and this type is shaped for the middle one:
//!
//! 1. **Mint and tolerate** (this change). Every path that can name a product does; a
//!    token without the claim is accepted exactly as it is today.
//! 2. **Classify.** The remaining unclassified client IDs are split per product in
//!    configuration — no code change, see [`crate::auth::client_ids`].
//! 3. **Require.** Absence becomes a 401 once no live session predates stage 1.
//!
//! Skipping to stage 3 signs every device on the estate out at once, which is why
//! `Product` is never mandatory in a claim and why the enforcement point treats `None` as
//! "permit, and say so in the log" rather than "deny".

use serde::{Deserialize, Serialize};
use std::fmt;

/// The product a session belongs to.
///
/// Serialized in its wire form (`teddy_fyi` / `scribbleroute`) in both places it is
/// stored: the JWT claim and the `sessions.product` column. Those two must agree, so they
/// share this representation rather than each spelling it out.
/// Each variant is renamed explicitly rather than through `rename_all`, because a derived
/// rule and [`Product::as_wire`] are two spellings of the same string and the derived one
/// is invisible: `rename_all = "snake_case"` silently produces `scribble_route`, which
/// would store one value and mint the other. `serde_uses_the_wire_form` below is what
/// noticed, and is what keeps the two in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Product {
    /// The household grocery and todo apps. Owns `Grocery`, `Todo` and — because it runs
    /// both of those futures and nothing else — `All`.
    #[serde(rename = "teddy_fyi")]
    TeddyFyi,
    /// ScribbleKeep / ScribbleBox. Owns the three `Scribble*` scopes.
    #[serde(rename = "scribbleroute")]
    ScribbleRoute,
}

impl Product {
    /// The stored and transmitted spelling. Kept as one function so the column and the
    /// claim cannot drift; `from_wire` is its inverse.
    pub fn as_wire(self) -> &'static str {
        match self {
            Product::TeddyFyi => "teddy_fyi",
            Product::ScribbleRoute => "scribbleroute",
        }
    }

    /// Parses the wire form. Returns `None` for anything unrecognised rather than
    /// erroring: the value can come from a `sessions` row written by a *newer* binary
    /// during a rollout, and the honest reading of "a product this build has never heard
    /// of" is the same as "no product claim" — unknown, so not enforceable.
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "teddy_fyi" => Some(Product::TeddyFyi),
            "scribbleroute" => Some(Product::ScribbleRoute),
            _ => None,
        }
    }
}

impl fmt::Display for Product {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_wire())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_form_round_trips() {
        for product in [Product::TeddyFyi, Product::ScribbleRoute] {
            assert_eq!(Product::from_wire(product.as_wire()), Some(product));
        }
    }

    /// The rollout property: a value this build does not know is `None`, not a panic and
    /// not a wrong guess. See the module docs on why absence must stay survivable.
    #[test]
    fn unknown_wire_values_are_none() {
        assert_eq!(Product::from_wire("something_else"), None);
        assert_eq!(Product::from_wire(""), None);
        assert_eq!(Product::from_wire("TEDDY_FYI"), None);
    }

    /// The claim and the column share one spelling, so the JSON form is pinned here too.
    #[test]
    fn serde_uses_the_wire_form() {
        for product in [Product::TeddyFyi, Product::ScribbleRoute] {
            let json = serde_json::to_string(&product).unwrap();
            assert_eq!(
                json,
                format!("\"{}\"", product.as_wire()),
                "the JWT claim and `as_wire` must serialize one product to one string"
            );
            assert_eq!(serde_json::from_str::<Product>(&json).unwrap(), product);
        }
    }
}
