//! The local-development sign-in bypass, and the compile-time wall around it.
//!
//! `POST /auth/login` normally proves who the caller is by validating a Google ID token.
//! Local development has no Google credentials to hand, so historically the handler
//! accepted any token beginning `mock.` and simply believed the `user_id` in the request
//! body. That is a total authentication bypass: it mints a session for *any* account a
//! caller names, and the account identifiers are not secret (see the note on
//! `parse_or_hash_uuid` in `src/routes/sync/remote_mutations.rs` — a user's config UUID is
//! a v5 hash of a Google `sub`, so it is publicly computable).
//!
//! Two things were wrong with how it used to be gated, and this module fixes both.
//!
//! **1. It was gated on `COOKIE_DOMAIN` being empty.** Whether to put a `Domain` attribute
//! on the session cookie is a deployment detail — an empty value is the correct, ordinary
//! configuration for a single-host deployment — while whether to accept unverified logins
//! is a security boundary. Coupling them meant a legitimate cookie setting silently turned
//! on impersonation of every account. Nothing in this module reads `cookie_domain`, and
//! nothing ever should: the two concerns are now completely independent.
//!
//! **2. The bypass shipped in the production binary.** Unreachable-by-configuration is a
//! weaker property than absent. The gate is therefore a cargo feature, `dev-auth`, which
//! the release build (`cargo build --release`, which is what the `Dockerfile` runs) does
//! not enable. With the feature off, the accepting branch below does not exist in the
//! compiled artifact at all — there is no runtime flag, environment variable or request
//! header that can bring it back.
//!
//! Local development enables it: `make dev` / `scripts/dev.sh` pass `--features dev-auth`.
//! See the README.

use std::collections::HashSet;

/// The email stamped on a bypassed login. Deliberately obvious in the `users` table, so a
/// row created this way is recognisable if a dev database is ever confused for a real one.
#[cfg(feature = "dev-auth")]
pub const DEV_USER_EMAIL: &str = "dev-user@teddy.fyi";

/// The token prefix that triggers the bypass.
#[cfg(feature = "dev-auth")]
pub const DEV_TOKEN_PREFIX: &str = "mock.";

/// Resolves the caller's identity *without* verifying anything, when this build was
/// compiled with `dev-auth` and the caller presented a `mock.` token.
///
/// Returns `None` in every other case, which sends `login_handler` down the real Google
/// validation path. In a build without the feature that is the only behaviour that exists:
/// the sibling function below has no body that can say yes.
#[cfg(feature = "dev-auth")]
pub fn dev_bypass_identity(
    google_auth_token: &str,
    requested_user_id: &str,
) -> Option<(String, Option<String>)> {
    if google_auth_token.starts_with(DEV_TOKEN_PREFIX) {
        // Loud on purpose. A `dev-auth` binary is not supposed to be anywhere a request can
        // reach it from outside a laptop, so every use of this path is worth a log line.
        tracing::warn!(
            user_id = %requested_user_id,
            "dev-auth: accepting an unverified `mock.` login. This build must never be deployed."
        );
        return Some((requested_user_id.to_string(), Some(DEV_USER_EMAIL.to_string())));
    }
    None
}

/// Production build: there is no bypass. Every login goes through Google, whatever the
/// token looks like and whatever `COOKIE_DOMAIN` is set to.
#[cfg(not(feature = "dev-auth"))]
pub fn dev_bypass_identity(
    _google_auth_token: &str,
    _requested_user_id: &str,
) -> Option<(String, Option<String>)> {
    None
}

/// Startup assertion over the Google audience allowlist, run once from `init_app_state`.
///
/// This is where the two builds are held to opposite standards, because a misconfiguration
/// means something different in each:
///
/// * **Without `dev-auth` (the shipped binary).** An empty allowlist means every real login
///   fails the audience check for the lifetime of the process, and previously the only
///   evidence was one `tracing::error!` line at boot. A process that cannot authenticate
///   anybody is not serving; failing to start turns a silent, total outage into a failed
///   rollout that a deploy actually blocks on. The prod deployment supplies its ID through
///   the legacy `SCRIBBLEROUTE_API_CLIENT_ID` var, which `load_google_client_ids` folds in,
///   so this is already satisfied there.
/// * **With `dev-auth`.** An empty allowlist is the normal, expected local setup — the
///   `mock.` path needs no client ID. What is *not* normal is a dev-auth binary that also
///   carries real Google client IDs: that combination is exactly what a development build
///   escaping onto a real deployment would look like, and it is worth refusing to start
///   over. A developer who genuinely needs to exercise real Google sign-in locally should
///   build without the feature, which is the same binary shape production runs.
///
/// Panicking is the right shape for both: this runs inside `init_app_state`, before the
/// listener is bound, so it can only ever abort a start-up, never fail a live request.
pub fn assert_startup_config(google_client_ids: &HashSet<String>) {
    #[cfg(feature = "dev-auth")]
    {
        assert!(
            google_client_ids.is_empty(),
            "Refusing to start: this binary was built with the `dev-auth` feature, which accepts \
             unverified `mock.` login tokens, but {} real Google client ID(s) are configured. \
             That combination looks like a development build pointed at a real deployment. \
             Build without `--features dev-auth` to use real Google sign-in.",
            google_client_ids.len()
        );
    }

    #[cfg(not(feature = "dev-auth"))]
    {
        assert!(
            !google_client_ids.is_empty(),
            "Refusing to start: no Google client IDs are configured, so every login would fail \
             the audience check. Set GOOGLE_CLIENT_IDS or GOOGLE_IOS_CLIENT_IDS (the legacy \
             GOOGLE_CLIENT_ID, GOOGLE_CLIENT_ID_GROCERY_WEB and SCRIBBLEROUTE_API_CLIENT_ID are \
             still honoured). For local development without Google credentials, build with \
             `--features dev-auth`, which is what `make dev` does."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The property that is the whole point of the feature gate, asserted in *both*
    /// configurations so neither can regress unnoticed: with `dev-auth` off a `mock.` token
    /// is just a token, and with it on the bypass fires. `COOKIE_DOMAIN` is nowhere in this
    /// decision — it is not an input to the function at all, which is the structural version
    /// of "decoupled".
    #[test]
    fn mock_tokens_are_accepted_only_when_the_feature_is_compiled_in() {
        let identity = dev_bypass_identity("mock.token", "victim-user-id");

        #[cfg(feature = "dev-auth")]
        assert_eq!(
            identity,
            Some((
                "victim-user-id".to_string(),
                Some(DEV_USER_EMAIL.to_string())
            ))
        );

        #[cfg(not(feature = "dev-auth"))]
        assert_eq!(identity, None, "the production build must have no bypass");
    }

    #[test]
    fn a_real_looking_token_never_takes_the_bypass() {
        assert_eq!(dev_bypass_identity("eyJhbGciOi.payload.sig", "user"), None);
        // Not a substring match: the marker has to start the token.
        assert_eq!(dev_bypass_identity("not-a-mock.token", "user"), None);
        assert_eq!(dev_bypass_identity("", "user"), None);
    }

    #[test]
    fn startup_config_accepts_the_shape_this_build_is_meant_to_run_in() {
        #[cfg(feature = "dev-auth")]
        assert_startup_config(&HashSet::new());

        #[cfg(not(feature = "dev-auth"))]
        assert_startup_config(&HashSet::from(["a-client-id".to_string()]));
    }

    #[test]
    #[should_panic(expected = "Refusing to start")]
    fn startup_config_rejects_the_other_shape() {
        // A dev-auth build carrying production credentials; or, without the feature, a
        // production build that could never authenticate anybody.
        #[cfg(feature = "dev-auth")]
        assert_startup_config(&HashSet::from(["a-client-id".to_string()]));

        #[cfg(not(feature = "dev-auth"))]
        assert_startup_config(&HashSet::new());
    }
}
