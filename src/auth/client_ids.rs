//! The Google client IDs this service accepts as an ID token audience, and which product
//! each of them belongs to.
//!
//! This used to be a flat `HashSet<String>`: every configured ID unioned together, and
//! membership was the whole check. That is the right shape for one product and the wrong
//! one for two. `login_handler` knows the audience of the token it just validated, which
//! is the *only* moment this service can tell a ScribbleRoute sign-in from a teddy.fyi
//! one — after that the audience is gone and every session looks alike. So the set becomes
//! a map, and the product it yields is what
//! [`crate::auth::tokens::Claims::product`] carries forward.
//!
//! # Configuration
//!
//! Two vars classify, and are the ones to prefer:
//!
//! * `TEDDY_FYI_CLIENT_IDS` — comma-separated, every client ID belonging to the grocery
//!   and todo apps, iOS included.
//! * `SCRIBBLEROUTE_CLIENT_IDS` — the same for ScribbleKeep / ScribbleBox.
//!
//! Four legacy vars are still honoured so no deployment breaks, and two of them classify
//! themselves by name:
//!
//! | Var | Product |
//! |---|---|
//! | `GOOGLE_CLIENT_ID_GROCERY_WEB` | teddy.fyi |
//! | `SCRIBBLEROUTE_API_CLIENT_ID` | ScribbleRoute |
//! | `GOOGLE_CLIENT_ID` | unclassified |
//! | `GOOGLE_CLIENT_IDS` | unclassified |
//! | `GOOGLE_IOS_CLIENT_IDS` | unclassified |
//!
//! **The three unclassified rows are a deliberate gap, not an oversight.**
//! `GOOGLE_IOS_CLIENT_IDS` is one comma-separated secret holding *both* products' iOS
//! apps, and nothing in this repository records which ID is which — that fact lives in
//! Secret Manager and in somebody's head. Guessing would be worse than not knowing: a
//! wrong guess mints a claim that then *denies* a legitimate device its own scopes, which
//! is an outage wearing a security feature's clothes. So an ID that arrives only through
//! an unclassified var stays unclassified, its sessions carry no product claim, and the
//! scope check waves them through while logging that it did.
//!
//! Closing that gap is a configuration change with no deploy: list the IDs under the two
//! per-product vars above, and every session established afterwards is classified.
//! [`ClientCatalog::unclassified`] is logged at start-up precisely so the operator can see
//! which IDs are still waiting for that.

use crate::auth::product::Product;
use std::collections::{HashMap, HashSet};

/// Environment variables that each hold a single client ID. These predate the
/// list-valued vars below and are still honored so existing deployments keep
/// working; new clients should go in one of the per-product lists instead.
const UNCLASSIFIED_SINGLE_ID_ENV_VARS: &[&str] = &["GOOGLE_CLIENT_ID"];

/// Legacy single-ID vars whose *name* already states the product, so they classify
/// without any new configuration. These are what makes this change worth something on the
/// day it deploys rather than on the day somebody edits a secret.
const CLASSIFIED_SINGLE_ID_ENV_VARS: &[(&str, Product)] = &[
    ("GOOGLE_CLIENT_ID_GROCERY_WEB", Product::TeddyFyi),
    ("SCRIBBLEROUTE_API_CLIENT_ID", Product::ScribbleRoute),
];

/// Comma-separated, per-product client IDs. The preferred configuration: an ID listed
/// here needs no name-based inference and no iOS special case.
const CLASSIFIED_LIST_ENV_VARS: &[(&str, Product)] = &[
    ("TEDDY_FYI_CLIENT_IDS", Product::TeddyFyi),
    ("SCRIBBLEROUTE_CLIENT_IDS", Product::ScribbleRoute),
];

/// Comma-separated client IDs of the iOS apps.
///
/// Google's iOS sign-in flow issues ID tokens whose `aud` is the app's own
/// client ID, so every entry here is also accepted as an audience. It spans both
/// products, which is why it cannot classify.
const IOS_CLIENT_IDS_ENV_VAR: &str = "GOOGLE_IOS_CLIENT_IDS";

/// Comma-separated client IDs for everything that is not an iOS app.
const OTHER_CLIENT_IDS_ENV_VAR: &str = "GOOGLE_CLIENT_IDS";

/// Every client ID accepted as an ID token audience, and the product it belongs to when
/// that is known.
///
/// `None` is a first-class answer here, not a failure: see the module docs. The map is
/// built once at start-up and read on the login and device-claim paths, so it is behind an
/// `Arc` in [`crate::state::AppState`] rather than cloned per request.
#[derive(Debug, Clone, Default)]
pub struct ClientCatalog {
    ids: HashMap<String, Option<Product>>,
}

impl ClientCatalog {
    /// Whether `aud` is an accepted audience. This is the check `login_handler` and the
    /// device claim path make, and it is exactly the old `HashSet::contains` — widening or
    /// narrowing *who may sign in* is not what this module changed.
    pub fn is_allowed(&self, aud: &str) -> bool {
        self.ids.contains_key(aud)
    }

    /// The product `aud` belongs to, or `None` when the audience is accepted but
    /// unclassified — or not accepted at all. Callers must check [`Self::is_allowed`]
    /// first; this is not an authorization decision.
    pub fn product_for(&self, aud: &str) -> Option<Product> {
        self.ids.get(aud).copied().flatten()
    }

    /// How many audiences are configured. Start-up asserts on this being zero or non-zero
    /// depending on the build; see [`crate::auth::dev_bypass::assert_startup_config`].
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The accepted audiences that carry no product. Logged at start-up so the remaining
    /// classification work is visible from a pod's first hundred lines rather than from
    /// reading this file.
    pub fn unclassified(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self
            .ids
            .iter()
            .filter(|(_, product)| product.is_none())
            .map(|(id, _)| id.as_str())
            .collect();
        ids.sort_unstable();
        ids
    }

    /// How many audiences are classified per product. Start-up logging only.
    pub fn classified_count(&self, product: Product) -> usize {
        self.ids
            .values()
            .filter(|assigned| **assigned == Some(product))
            .count()
    }

    /// Builds a catalog from already-parsed IDs. Split out from [`load_client_catalog`] so
    /// every rule below is testable without mutating process-wide environment state.
    ///
    /// Classification wins over its absence regardless of the order the sources are given
    /// in — an ID named by both `SCRIBBLEROUTE_CLIENT_IDS` and the mixed
    /// `GOOGLE_IOS_CLIENT_IDS` is a ScribbleRoute ID that also happens to be an iOS app,
    /// which is the ordinary case and not a conflict.
    ///
    /// # Panics
    ///
    /// If one ID is claimed by *both* products. That is a configuration statement which
    /// cannot be honoured — whichever product lost would have its devices denied their own
    /// scopes — and it is unambiguous enough to be worth refusing to start over. Like the
    /// other start-up assertions this can only ever fail a rollout, never a live request.
    pub fn build(
        classified: impl IntoIterator<Item = (String, Product)>,
        unclassified: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut ids: HashMap<String, Option<Product>> = HashMap::new();

        for (id, product) in classified {
            match ids.get(&id) {
                Some(Some(existing)) if *existing != product => {
                    panic!(
                        "Refusing to start: Google client ID {id} is configured as both \
                         {existing} and {product}. One client ID belongs to one product; \
                         listing it twice would deny one of them its own sync scopes. Remove \
                         it from whichever of TEDDY_FYI_CLIENT_IDS / SCRIBBLEROUTE_CLIENT_IDS \
                         is wrong."
                    );
                }
                _ => {
                    ids.insert(id, Some(product));
                }
            }
        }

        for id in unclassified {
            ids.entry(id).or_insert(None);
        }

        Self { ids }
    }

    /// A catalog whose every audience is unclassified. The pre-split shape, kept for tests
    /// and for the dev bypass, where no Google audience is involved at all.
    pub fn from_unclassified(ids: impl IntoIterator<Item = String>) -> Self {
        Self::build(Vec::new(), ids)
    }
}

/// Reads the environment into a [`ClientCatalog`].
///
/// Returns an empty catalog when nothing is configured, which makes every real login fail
/// the audience check in `auth::handlers::login_handler` — and which start-up refuses to
/// proceed with in a non-`dev-auth` build.
pub fn load_client_catalog() -> ClientCatalog {
    let classified = CLASSIFIED_SINGLE_ID_ENV_VARS
        .iter()
        .chain(CLASSIFIED_LIST_ENV_VARS.iter())
        .flat_map(|(name, product)| {
            parse_client_ids(std::env::var(name).ok())
                .into_iter()
                .map(move |id| (id, *product))
        })
        .collect::<Vec<_>>();

    let unclassified = UNCLASSIFIED_SINGLE_ID_ENV_VARS
        .iter()
        .chain(std::iter::once(&OTHER_CLIENT_IDS_ENV_VAR))
        .chain(std::iter::once(&IOS_CLIENT_IDS_ENV_VAR))
        .filter_map(|name| std::env::var(name).ok());

    ClientCatalog::build(classified, parse_client_ids(unclassified))
}

/// Splits and trims comma-separated raw env values, dropping empties. Split out
/// from the loaders so it is testable without mutating process-wide environment
/// state.
fn parse_client_ids<I: IntoIterator<Item = String>>(env_values: I) -> HashSet<String> {
    env_values
        .into_iter()
        .flat_map(|raw| {
            raw.split(',')
                .map(|id| id.trim().to_string())
                .collect::<Vec<_>>()
        })
        .filter(|id| !id.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_config_yields_an_empty_set() {
        assert!(parse_client_ids(Vec::new()).is_empty());
    }

    #[test]
    fn single_value_vars_are_collected() {
        let ids = parse_client_ids(vec![
            "grocery-ios".to_string(),
            "grocery-web".to_string(),
        ]);

        assert!(ids.contains("grocery-ios"));
        assert!(ids.contains("grocery-web"));
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn comma_separated_values_are_split_and_trimmed() {
        let ids = parse_client_ids(vec![" a-client , b-client ,, ".to_string()]);

        assert!(ids.contains("a-client"));
        assert!(ids.contains("b-client"));
        assert!(!ids.contains(""));
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn duplicate_ids_are_deduplicated() {
        let ids = parse_client_ids(vec![
            "shared-client".to_string(),
            "shared-client, extra-client".to_string(),
        ]);

        assert_eq!(ids.len(), 2);
    }

    /// Guards the rule that makes iOS sign-in work: an iOS client ID is its own
    /// audience, so it must survive into the accepted-audience set.
    #[test]
    fn ios_client_ids_are_accepted_as_audiences() {
        let ios = parse_client_ids(vec!["grocery-ios, scribblebox-ios".to_string()]);
        let catalog =
            ClientCatalog::build(Vec::new(), ios.iter().cloned().chain(["grocery-web".to_string()]));

        for id in &ios {
            assert!(catalog.is_allowed(id), "iOS client {id} is not an audience");
        }
        assert!(catalog.is_allowed("grocery-web"));
        assert_eq!(catalog.len(), 3);
    }

    #[test]
    fn ios_clients_listed_in_another_var_are_not_duplicated() {
        let catalog = ClientCatalog::from_unclassified(vec![
            "grocery-ios".to_string(),
            "grocery-ios".to_string(),
        ]);

        assert_eq!(catalog.len(), 1);
    }

    #[test]
    fn ios_clients_alone_are_enough_to_form_the_audience_set() {
        let catalog = ClientCatalog::from_unclassified(vec!["grocery-ios".to_string()]);

        assert!(catalog.is_allowed("grocery-ios"));
        assert_eq!(catalog.len(), 1);
    }

    #[test]
    fn classified_ids_report_their_product() {
        let catalog = ClientCatalog::build(
            vec![
                ("grocery-web".to_string(), Product::TeddyFyi),
                ("scribbleroute-api".to_string(), Product::ScribbleRoute),
            ],
            Vec::new(),
        );

        assert_eq!(catalog.product_for("grocery-web"), Some(Product::TeddyFyi));
        assert_eq!(
            catalog.product_for("scribbleroute-api"),
            Some(Product::ScribbleRoute)
        );
        assert_eq!(catalog.classified_count(Product::TeddyFyi), 1);
        assert_eq!(catalog.classified_count(Product::ScribbleRoute), 1);
    }

    /// The gap this module is honest about: an audience that only ever arrived through a
    /// mixed var is accepted and unclassified, and says so.
    #[test]
    fn ids_from_mixed_vars_stay_unclassified() {
        let catalog = ClientCatalog::build(
            vec![("grocery-web".to_string(), Product::TeddyFyi)],
            vec!["mystery-ios".to_string()],
        );

        assert!(catalog.is_allowed("mystery-ios"));
        assert_eq!(catalog.product_for("mystery-ios"), None);
        assert_eq!(catalog.unclassified(), vec!["mystery-ios"]);
    }

    /// A classified ID that *also* appears in the mixed iOS list is the ordinary case —
    /// an iOS app someone has taken the trouble to assign — and must not be demoted.
    #[test]
    fn classification_survives_the_same_id_appearing_unclassified() {
        let catalog = ClientCatalog::build(
            vec![("scribblekeep-ios".to_string(), Product::ScribbleRoute)],
            vec!["scribblekeep-ios".to_string()],
        );

        assert_eq!(
            catalog.product_for("scribblekeep-ios"),
            Some(Product::ScribbleRoute)
        );
        assert!(catalog.unclassified().is_empty());
        assert_eq!(catalog.len(), 1);
    }

    #[test]
    fn an_unknown_audience_has_no_product_and_is_not_allowed() {
        let catalog = ClientCatalog::build(
            vec![("grocery-web".to_string(), Product::TeddyFyi)],
            Vec::new(),
        );

        assert!(!catalog.is_allowed("someone-elses-app"));
        assert_eq!(catalog.product_for("someone-elses-app"), None);
    }

    /// Two products claiming one ID cannot be honoured, so it fails the start-up rather
    /// than silently denying one product its own scopes.
    #[test]
    #[should_panic(expected = "configured as both")]
    fn an_id_claimed_by_both_products_refuses_to_start() {
        ClientCatalog::build(
            vec![
                ("shared-ios".to_string(), Product::TeddyFyi),
                ("shared-ios".to_string(), Product::ScribbleRoute),
            ],
            Vec::new(),
        );
    }

    /// The same ID listed twice for the same product is a duplicate, not a conflict.
    #[test]
    fn an_id_listed_twice_for_one_product_is_fine() {
        let catalog = ClientCatalog::build(
            vec![
                ("grocery-web".to_string(), Product::TeddyFyi),
                ("grocery-web".to_string(), Product::TeddyFyi),
            ],
            Vec::new(),
        );

        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog.product_for("grocery-web"), Some(Product::TeddyFyi));
    }
}
