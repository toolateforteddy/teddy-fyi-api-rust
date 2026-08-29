use std::collections::HashSet;

/// Environment variables that each hold a single client ID. These predate the
/// list-valued vars below and are still honored so existing deployments keep
/// working; new clients should go in one of the lists instead.
const SINGLE_ID_ENV_VARS: &[&str] = &[
    "GOOGLE_CLIENT_ID",
    "GOOGLE_CLIENT_ID_GROCERY_WEB",
    "SCRIBBLEROUTE_API_CLIENT_ID",
];

/// Comma-separated client IDs of the iOS apps.
///
/// Google's iOS sign-in flow issues ID tokens whose `aud` is the app's own
/// client ID, so every entry here is also accepted as an audience.
const IOS_CLIENT_IDS_ENV_VAR: &str = "GOOGLE_IOS_CLIENT_IDS";

/// Comma-separated client IDs for everything that is not an iOS app.
const OTHER_CLIENT_IDS_ENV_VAR: &str = "GOOGLE_CLIENT_IDS";

/// Builds the set of client IDs accepted as an ID token audience: the iOS
/// clients, whose IDs double as their own audience, plus every other
/// configured client.
///
/// Returns an empty set when nothing is configured, which makes every real
/// login fail the audience check in `auth::handlers::login_handler`.
pub fn load_google_client_ids() -> HashSet<String> {
    let ios_client_ids = parse_client_ids(std::env::var(IOS_CLIENT_IDS_ENV_VAR).ok());

    let others = SINGLE_ID_ENV_VARS
        .iter()
        .chain(std::iter::once(&OTHER_CLIENT_IDS_ENV_VAR))
        .filter_map(|name| std::env::var(name).ok());

    build_audience_set(ios_client_ids, others)
}

/// Unions the iOS client IDs into the audience set alongside every other
/// configured client. Split out from [`load_google_client_ids`] so the rule is
/// testable without mutating process-wide environment state.
fn build_audience_set<I: IntoIterator<Item = String>>(
    ios_client_ids: HashSet<String>,
    other_env_values: I,
) -> HashSet<String> {
    let mut audiences = ios_client_ids;
    audiences.extend(parse_client_ids(other_env_values));
    audiences
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
        let audiences = build_audience_set(ios.clone(), vec!["grocery-web".to_string()]);

        for id in &ios {
            assert!(audiences.contains(id), "iOS client {id} is not an audience");
        }
        assert!(audiences.contains("grocery-web"));
        assert_eq!(audiences.len(), 3);
    }

    #[test]
    fn ios_clients_listed_in_another_var_are_not_duplicated() {
        let ios = parse_client_ids(vec!["grocery-ios".to_string()]);
        let audiences = build_audience_set(ios, vec!["grocery-ios".to_string()]);

        assert_eq!(audiences.len(), 1);
    }

    #[test]
    fn ios_clients_alone_are_enough_to_form_the_audience_set() {
        let ios = parse_client_ids(vec!["grocery-ios".to_string()]);
        let audiences = build_audience_set(ios, Vec::new());

        assert!(audiences.contains("grocery-ios"));
        assert_eq!(audiences.len(), 1);
    }
}
