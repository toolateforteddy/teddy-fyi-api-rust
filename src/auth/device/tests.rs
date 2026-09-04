use super::*;
use crate::routes::sync::tests::helpers::setup_state;
use axum::extract::{Json, State};
use axum::http::StatusCode;
use sqlx::PgPool;

/// Reads a handler's JSON body. Every device endpoint that returns one returns it small.
async fn body_json(response: Response) -> serde_json::Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("body should be readable");
    serde_json::from_slice(&bytes).expect("body should be JSON")
}

/// Runs `/start` and hands back the device code and the normalized user code, which is
/// what every later step needs.
async fn start(state: &AppState, client_uuid: &str) -> (String, String) {
    let response = start_handler(
        State(state.clone()),
        Json(StartRequest {
            client_uuid: client_uuid.to_string(),
            app: Some("scribblekeep".to_string()),
        }),
    )
    .await
    .expect("start should succeed");

    let body = body_json(response).await;
    let device_code = body["device_code"].as_str().unwrap().to_string();
    let user_code = normalize_user_code(body["user_code"].as_str().unwrap());
    (device_code, user_code)
}

/// A poll's `last_polled_at` is what the interval limit reads. Real tablets sleep between
/// polls; tests that are exercising something other than pacing rewind it instead.
async fn forget_last_poll(pool: &PgPool, client_uuid: &str) {
    sqlx::query!(
        "UPDATE device_authorizations SET last_polled_at = NULL WHERE client_uuid = $1",
        client_uuid
    )
    .execute(pool)
    .await
    .unwrap();
}

#[test]
fn user_code_alphabet_is_unambiguous_and_voiceless() {
    // Pinned to the literal in the spec, not just to the rules behind it: the website's
    // entry field is written from that same literal, and a code either side rejects is a
    // parent who cannot pair their tablet.
    assert_eq!(USER_CODE_ALPHABET, b"23456789CDFHJKMNPQRTVWXY");
    assert_eq!(USER_CODE_ALPHABET.len(), 24);
    for excluded in b"0O1ILAEIOUBGSZ" {
        assert!(
            !USER_CODE_ALPHABET.contains(excluded),
            "{} should not be in the user code alphabet",
            *excluded as char
        );
    }

    let code = generate_user_code();
    assert_eq!(code.len(), USER_CODE_LEN);
    assert!(code.bytes().all(|b| USER_CODE_ALPHABET.contains(&b)));
}

#[test]
fn user_codes_round_trip_through_display_form() {
    let code = generate_user_code();
    let displayed = format_user_code(&code);

    assert_eq!(displayed.len(), USER_CODE_LEN + 1);
    assert_eq!(&displayed[4..5], "-");
    assert_eq!(normalize_user_code(&displayed), code);
    // What a parent actually types: lowercase, spaced, dash forgotten.
    assert_eq!(
        normalize_user_code(&format!(" {} {} ", &code[..4].to_lowercase(), &code[4..])),
        code
    );
}

/// The audience rule `/claim` shares with `login_handler`. A token minted for another
/// client is rejected however valid its signature is.
#[test]
fn audience_must_be_configured() {
    let allowed = ["test-scribbleroute-client".to_string()]
        .into_iter()
        .collect();

    assert!(audience_is_allowed(&allowed, "test-scribbleroute-client"));
    assert!(!audience_is_allowed(&allowed, "some-other-client"));
}

#[test]
fn poll_interval_permits_a_first_poll_and_paces_the_rest() {
    let now = Utc::now();
    assert!(!polled_too_soon(None, now));
    assert!(polled_too_soon(Some(now - Duration::seconds(1)), now));
    assert!(!polled_too_soon(
        Some(now - Duration::seconds(POLL_INTERVAL_SECS + 1)),
        now
    ));
}

/// The whole handshake: the tablet starts, the parent claims, the tablet collects a
/// session it can sync with.
#[sqlx::test]
async fn happy_path_pairs_the_tablet(pool: PgPool) {
    let state = setup_state(pool.clone());
    let client_uuid = "fire-tablet-1";
    let (device_code, user_code) = start(&state, client_uuid).await;

    let claimed = claim_for_user(&state, "google-sub-1", Some("parent@example.com"), &user_code)
        .await
        .expect("claim should succeed");
    assert_eq!(claimed, StatusCode::NO_CONTENT);

    let response = poll_handler(
        State(state.clone()),
        Json(PollRequest {
            device_code,
            client_uuid: client_uuid.to_string(),
        }),
    )
    .await
    .expect("poll should succeed");
    assert_eq!(response.status(), StatusCode::OK);

    let body = body_json(response).await;
    assert!(!body["access_token"].as_str().unwrap().is_empty());
    assert!(!body["refresh_token"].as_str().unwrap().is_empty());

    // The session is the same shape `/auth/login` writes, keyed to the tablet.
    let session = sqlx::query!(
        "SELECT user_id FROM sessions WHERE user_id = $1 AND client_uuid = $2",
        "google-sub-1",
        client_uuid
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(session.user_id, "google-sub-1");

    // The parent's account exists with the email the Google token carried.
    let user = sqlx::query!("SELECT email FROM users WHERE id = $1", "google-sub-1")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(user.email.as_deref(), Some("parent@example.com"));
}

/// Claiming a code nobody issued is a `404`, and it costs the caller one of their attempts.
#[sqlx::test]
async fn unknown_user_code_is_not_found(pool: PgPool) {
    let state = setup_state(pool.clone());

    let result = claim_for_user(&state, "google-sub-1", None, "CDFH-JKMN").await;
    assert_eq!(result.unwrap_err(), StatusCode::NOT_FOUND);

    let failures = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM device_claim_failures WHERE user_id = $1"#,
        "google-sub-1"
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(failures, 1);
}

/// A code past `expires_at` is indistinguishable from one that never existed, and the
/// tablet still polling gets a terminal `410` rather than waiting forever.
#[sqlx::test]
async fn expired_code_cannot_be_claimed_or_polled(pool: PgPool) {
    let state = setup_state(pool.clone());
    let client_uuid = "fire-tablet-expired";
    let (device_code, user_code) = start(&state, client_uuid).await;

    sqlx::query!(
        "UPDATE device_authorizations SET expires_at = now() - interval '1 minute' WHERE client_uuid = $1",
        client_uuid
    )
    .execute(&pool)
    .await
    .unwrap();

    let claim = claim_for_user(&state, "google-sub-1", None, &user_code).await;
    assert_eq!(claim.unwrap_err(), StatusCode::NOT_FOUND);

    let poll = poll_handler(
        State(state.clone()),
        Json(PollRequest {
            device_code,
            client_uuid: client_uuid.to_string(),
        }),
    )
    .await;
    assert_eq!(poll.unwrap_err(), StatusCode::GONE);
}

/// A device code is single-use: whoever collects the session first spends it, and a replay
/// — the tablet retrying, or someone who copied the code off the wire — gets nothing.
#[sqlx::test]
async fn device_code_cannot_be_replayed(pool: PgPool) {
    let state = setup_state(pool.clone());
    let client_uuid = "fire-tablet-replay";
    let (device_code, user_code) = start(&state, client_uuid).await;

    claim_for_user(&state, "google-sub-1", None, &user_code)
        .await
        .unwrap();

    let first = poll_handler(
        State(state.clone()),
        Json(PollRequest {
            device_code: device_code.clone(),
            client_uuid: client_uuid.to_string(),
        }),
    )
    .await
    .expect("first poll should succeed");
    assert_eq!(first.status(), StatusCode::OK);

    forget_last_poll(&pool, client_uuid).await;

    let second = poll_handler(
        State(state.clone()),
        Json(PollRequest {
            device_code,
            client_uuid: client_uuid.to_string(),
        }),
    )
    .await;
    assert_eq!(second.unwrap_err(), StatusCode::GONE);
}

/// Before a parent has done anything, the tablet is told to keep waiting — never handed a
/// session, and never told the code is bad.
#[sqlx::test]
async fn poll_before_claim_is_pending(pool: PgPool) {
    let state = setup_state(pool.clone());
    let client_uuid = "fire-tablet-pending";
    let (device_code, _user_code) = start(&state, client_uuid).await;

    let response = poll_handler(
        State(state.clone()),
        Json(PollRequest {
            device_code,
            client_uuid: client_uuid.to_string(),
        }),
    )
    .await
    .expect("poll should succeed");

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(body_json(response).await["status"], "pending");
}

/// A device code lifted from one install is not portable to another, and the refusal looks
/// exactly like an unknown code so it is not an oracle for valid ones.
#[sqlx::test]
async fn client_uuid_mismatch_is_not_found(pool: PgPool) {
    let state = setup_state(pool.clone());
    let (device_code, user_code) = start(&state, "fire-tablet-owner").await;

    claim_for_user(&state, "google-sub-1", None, &user_code)
        .await
        .unwrap();

    let result = poll_handler(
        State(state.clone()),
        Json(PollRequest {
            device_code,
            client_uuid: "someone-elses-tablet".to_string(),
        }),
    )
    .await;

    assert_eq!(result.unwrap_err(), StatusCode::NOT_FOUND);
}

/// Polling faster than the advertised interval is refused, without disturbing the code.
#[sqlx::test]
async fn polling_faster_than_the_interval_is_refused(pool: PgPool) {
    let state = setup_state(pool.clone());
    let client_uuid = "fire-tablet-impatient";
    let (device_code, _user_code) = start(&state, client_uuid).await;

    let first = poll_handler(
        State(state.clone()),
        Json(PollRequest {
            device_code: device_code.clone(),
            client_uuid: client_uuid.to_string(),
        }),
    )
    .await
    .expect("first poll should succeed");
    assert_eq!(first.status(), StatusCode::ACCEPTED);

    let second = poll_handler(
        State(state.clone()),
        Json(PollRequest {
            device_code: device_code.clone(),
            client_uuid: client_uuid.to_string(),
        }),
    )
    .await;
    assert_eq!(second.unwrap_err(), StatusCode::TOO_MANY_REQUESTS);

    // Refused, not consumed: once the tablet waits, the code still works.
    forget_last_poll(&pool, client_uuid).await;
    let third = poll_handler(
        State(state.clone()),
        Json(PollRequest {
            device_code,
            client_uuid: client_uuid.to_string(),
        }),
    )
    .await
    .expect("poll after waiting should succeed");
    assert_eq!(third.status(), StatusCode::ACCEPTED);
}

/// Guessing costs something: five failures inside the window lock this Google account out
/// of claiming, even when it then guesses a code that is genuinely live.
#[sqlx::test]
async fn repeated_failures_lock_out_claiming(pool: PgPool) {
    let state = setup_state(pool.clone());
    let (_device_code, user_code) = start(&state, "fire-tablet-guessed-at").await;

    for _ in 0..MAX_CLAIM_FAILURES {
        let result = claim_for_user(&state, "guesser", None, "CDFH-JKMN").await;
        assert_eq!(result.unwrap_err(), StatusCode::NOT_FOUND);
    }

    let locked = claim_for_user(&state, "guesser", None, &user_code).await;
    assert_eq!(locked.unwrap_err(), StatusCode::TOO_MANY_REQUESTS);

    // The lockout is per account: another parent is unaffected.
    let other = claim_for_user(&state, "innocent-parent", None, &user_code)
        .await
        .expect("an unrelated account should still be able to claim");
    assert_eq!(other, StatusCode::NO_CONTENT);
}

/// Failures age out of the window, so a parent who fumbled the code an hour ago is not
/// still locked out.
#[sqlx::test]
async fn failures_outside_the_window_do_not_count(pool: PgPool) {
    let state = setup_state(pool.clone());
    let (_device_code, user_code) = start(&state, "fire-tablet-forgiven").await;

    for _ in 0..MAX_CLAIM_FAILURES {
        let _ = claim_for_user(&state, "fumbling-parent", None, "CDFH-JKMN").await;
    }
    sqlx::query!(
        "UPDATE device_claim_failures SET failed_at = now() - interval '1 hour' WHERE user_id = $1",
        "fumbling-parent"
    )
    .execute(&pool)
    .await
    .unwrap();

    let claimed = claim_for_user(&state, "fumbling-parent", None, &user_code)
        .await
        .expect("claim should succeed once the failures have aged out");
    assert_eq!(claimed, StatusCode::NO_CONTENT);
}

/// A code can only be redeemed once, so a second parent cannot quietly take over a tablet
/// that is mid-pairing.
#[sqlx::test]
async fn a_claimed_code_cannot_be_claimed_again(pool: PgPool) {
    let state = setup_state(pool.clone());
    let (_device_code, user_code) = start(&state, "fire-tablet-contested").await;

    claim_for_user(&state, "first-parent", None, &user_code)
        .await
        .unwrap();

    let second = claim_for_user(&state, "second-parent", None, &user_code).await;
    assert_eq!(second.unwrap_err(), StatusCode::NOT_FOUND);

    let row = sqlx::query!(
        "SELECT user_id, attempts FROM device_authorizations WHERE user_code = $1",
        user_code
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.user_id.as_deref(), Some("first-parent"));
    assert_eq!(row.attempts, 1);
}

/// The sweep drops what is dead and leaves a live pairing alone.
#[sqlx::test]
async fn reaper_drops_only_dead_rows(pool: PgPool) {
    let state = setup_state(pool.clone());
    start(&state, "fire-tablet-live").await;
    start(&state, "fire-tablet-stale").await;

    sqlx::query!(
        "UPDATE device_authorizations
            SET expires_at = now() - interval '2 days'
          WHERE client_uuid = $1",
        "fire-tablet-stale"
    )
    .execute(&pool)
    .await
    .unwrap();

    let summary = crate::jobs::reap_device_authorizations::reap_device_authorizations(&pool)
        .await
        .unwrap();
    assert_eq!(summary.authorizations_deleted, 1);

    let remaining = sqlx::query_scalar!(
        r#"SELECT client_uuid AS "client_uuid!" FROM device_authorizations"#
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(remaining, vec!["fire-tablet-live".to_string()]);
}

/// The `app` a client sends is a build's enum name, and it is the only thing standing
/// between a teddy.fyi parent and ScribbleRoute's pairing page. Folded rather than matched
/// literally so a client that spells its app with a dot or a space still lands.
#[test]
fn app_names_fold_to_one_spelling() {
    assert_eq!(normalize_app("TEDDY_FYI"), "TEDDY_FYI");
    assert_eq!(normalize_app("teddy.fyi"), "TEDDY_FYI");
    assert_eq!(normalize_app("  teddy fyi  "), "TEDDY_FYI");
    assert_eq!(normalize_app("Scribble-Keep"), "SCRIBBLE_KEEP");
}

/// Both products pair through this one service and redeem on their own websites, so the
/// table is pinned: a wrong entry here is a parent typing a live code into a page that has
/// never heard of it.
#[test]
fn each_app_redeems_on_its_own_site() {
    let uri = |app: &str| {
        APP_VERIFICATION_URIS
            .iter()
            .find(|(name, _)| *name == app)
            .map(|(_, uri)| *uri)
    };

    assert_eq!(uri("SCRIBBLE_KEEP"), Some("https://scribbleroute.com/link"));
    assert_eq!(uri("SCRIBBLE_BOX"), Some("https://scribbleroute.com/link"));
    assert_eq!(uri("TEDDY_FYI"), Some("https://teddy.fyi/link"));
    assert_eq!(uri("TEDDY_FYI_GROCERY"), Some("https://teddy.fyi/link"));

    for (app, _) in APP_VERIFICATION_URIS {
        assert_eq!(
            normalize_app(app),
            *app,
            "table keys are looked up after normalisation, so they must already be normal"
        );
    }
}

/// Resolution with nothing configured, which is how this runs in production: the table
/// answers for an app it knows, and anything else gets the default page rather than an
/// error.
#[test]
fn unconfigured_lookup_falls_back_to_the_default_page() {
    // Only meaningful with no deployment-level override in the ambient environment.
    if std::env::var("DEVICE_VERIFICATION_URI").is_ok() {
        return;
    }

    assert_eq!(
        verification_uri(Some("SCRIBBLE_KEEP")),
        "https://scribbleroute.com/link"
    );
    assert_eq!(verification_uri(Some("teddy_fyi")), "https://teddy.fyi/link");
    assert_eq!(
        verification_uri(Some("some-future-app")),
        DEFAULT_VERIFICATION_URI
    );
    assert_eq!(verification_uri(None), DEFAULT_VERIFICATION_URI);
}
