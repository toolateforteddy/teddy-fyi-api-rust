#[cfg(test)]
mod tests {
    use crate::auth::tokens::{create_access_token, hash_refresh_token, verify_refresh_token, Claims};
    use crate::auth::handlers::{
        login_handler, logout_handler, refresh_handler, LoginRequest, RefreshRequest,
        DEFAULT_SESSION_SECS, REFRESH_GRACE_SECS,
    };
    use crate::auth::tokens::ACCESS_TOKEN_TTL_SECS;
    use crate::routes::sync::tests::helpers::setup_state;
    use axum::extract::State;
    use axum::Json;
    use sqlx::PgPool;

    #[test]
    fn test_token_lifecycle() {
        let secret = b"super-secret-key-for-testing";
        let user_id = "user123";
        let client_uuid = "device-abc";

        // Test JWT creation
        let token = create_access_token(user_id, client_uuid, secret, None).unwrap();
        assert!(!token.is_empty());

        // Test Refresh token hashing
        let raw_refresh = "very-secret-refresh-token";
        let hash = hash_refresh_token(raw_refresh);

        assert!(verify_refresh_token(&hash, raw_refresh));
        assert!(!verify_refresh_token(&hash, "wrong-token"));
    }

    #[test]
    fn test_create_access_token_custom_expiration() {
        let secret = b"super-secret-key-for-testing";
        let user_id = "user123";
        let client_uuid = "device-abc";

        // 1. Test custom duration (60 seconds)
        let token_60 = create_access_token(user_id, client_uuid, secret, Some(60)).unwrap();
        let decoded_60 = jsonwebtoken::decode::<Claims>(
            &token_60,
            &jsonwebtoken::DecodingKey::from_secret(secret),
            &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
        ).unwrap();
        let now = chrono::Utc::now().timestamp() as usize;
        let diff = decoded_60.claims.exp - now;
        // Verify expiration is roughly 60 seconds from now
        assert!(diff >= 55 && diff <= 65);

        // 2. An over-large request is clamped to the ceiling, not honoured. The old
        //    behaviour clamped a day's worth of seconds to... a day, which is why this asks
        //    for the previous ceiling specifically: 24 hours must now come back as 15
        //    minutes, or the clamp is not doing anything.
        let token_large = create_access_token(user_id, client_uuid, secret, Some(86400)).unwrap();
        let decoded_large = jsonwebtoken::decode::<Claims>(
            &token_large,
            &jsonwebtoken::DecodingKey::from_secret(secret),
            &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
        ).unwrap();
        let diff_large = (decoded_large.claims.exp - now) as i64;
        assert!(
            (diff_large - ACCESS_TOKEN_TTL_SECS).abs() <= 10,
            "an 86400s request must clamp to the ceiling, got {diff_large}s"
        );

        // 3. Test negative duration defaults to the ceiling
        let token_neg = create_access_token(user_id, client_uuid, secret, Some(-100)).unwrap();
        let decoded_neg = jsonwebtoken::decode::<Claims>(
            &token_neg,
            &jsonwebtoken::DecodingKey::from_secret(secret),
            &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
        ).unwrap();
        let diff_neg = (decoded_neg.claims.exp - now) as i64;
        assert!((diff_neg - ACCESS_TOKEN_TTL_SECS).abs() <= 10);

        // 4. And no argument at all is the same ceiling — this is what device pairing mints
        //    with, since a tablet has no `expires_in_secs` to ask with.
        let token_default = create_access_token(user_id, client_uuid, secret, None).unwrap();
        let decoded_default = jsonwebtoken::decode::<Claims>(
            &token_default,
            &jsonwebtoken::DecodingKey::from_secret(secret),
            &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
        ).unwrap();
        let diff_default = (decoded_default.claims.exp - now) as i64;
        assert!((diff_default - ACCESS_TOKEN_TTL_SECS).abs() <= 10);
    }

    /// The default and the clamp are the same number by construction, and both are short.
    ///
    /// The pair used to be two independent literals that happened to agree; this pins the
    /// property, not the value, so lowering one without the other fails here rather than in
    /// production — where the symptom would be a `DEFAULT_SESSION_SECS` clamp that lets
    /// through a lifetime the minting function then quietly shortens, or worse, does not.
    #[test]
    fn test_session_ttl_is_short_and_the_clamp_is_not_a_lie() {
        assert_eq!(DEFAULT_SESSION_SECS, ACCESS_TOKEN_TTL_SECS);
        assert!(
            ACCESS_TOKEN_TTL_SECS <= 900,
            "the post-sign-out exposure window is this constant; keep it at 15 minutes or less"
        );
    }

    #[test]
    fn test_cookie_extraction_helper() {
        let cookie_str = "other_cookie=val; access_token=my_secret_jwt; another=cookie";
        let token = cookie_str.split(';')
            .map(|s| s.trim())
            .find(|s| s.starts_with("access_token="))
            .and_then(|s| s.strip_prefix("access_token="));
        assert_eq!(token, Some("my_secret_jwt"));
        
        let cookie_str_single = "access_token=token123";
        let token_single = cookie_str_single.split(';')
            .map(|s| s.trim())
            .find(|s| s.starts_with("access_token="))
            .and_then(|s| s.strip_prefix("access_token="));
        assert_eq!(token_single, Some("token123"));

        let cookie_str_missing = "other_cookie=val";
        let token_missing = cookie_str_missing.split(';')
            .map(|s| s.trim())
            .find(|s| s.starts_with("access_token="))
            .and_then(|s| s.strip_prefix("access_token="));
        assert_eq!(token_missing, None);
    }

    #[test]
    fn test_logout_cookie_clearing_value() {
        let cookie_header_value = "access_token=; HttpOnly; Secure; SameSite=Lax; Domain=.teddy.fyi; Path=/; Max-Age=0";
        let token = cookie_header_value.split(';')
            .map(|s| s.trim())
            .find(|s| s.starts_with("access_token="))
            .and_then(|s| s.strip_prefix("access_token="));
        assert_eq!(token, Some(""));
    }

    /// Only compiles in a `dev-auth` build, because a `mock.` token is the only way to reach
    /// `login_handler`'s success path without a real Google ID token. It no longer clears
    /// `cookie_domain` to get there: the bypass is a property of the build, not of the
    /// cookie configuration, so the state left here is the ordinary `.teddy.fyi` one.
    #[cfg(feature = "dev-auth")]
    #[sqlx::test]
    async fn test_login_handler_custom_duration(pool: PgPool) {
        let state = setup_state(pool.clone());

        // Login with 10 seconds custom expiration, requesting a cookie
        let payload = LoginRequest {
            user_id: "user-test-custom-duration".to_string(),
            client_uuid: "client-test-custom-duration".to_string(),
            google_auth_token: "mock.token".to_string(),
            use_cookie: Some(true),
            expires_in_secs: Some(10),
        };

        let response = login_handler(State(state), Json(payload))
            .await
            .expect("Login should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        // Verify Set-Cookie header contains Max-Age=10
        let cookie_header = response.headers().get(axum::http::header::SET_COOKIE).unwrap();
        let cookie_str = cookie_header.to_str().unwrap();
        assert!(cookie_str.contains("Max-Age=10"), "Set-Cookie should have Max-Age=10, got: {}", cookie_str);
    }

    #[sqlx::test]
    async fn test_refresh_handler_custom_duration_and_rotation(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-refresh-test";
        let client_uuid = "client-refresh-test";

        // Insert initial session
        let raw_refresh = "initial-refresh-token";
        let hash = hash_refresh_token(raw_refresh);
        let expiration = chrono::Utc::now() + chrono::Duration::days(1);

        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, NULL, NULL)",
            user_id,
            client_uuid,
            hash,
            expiration
        ).execute(&pool).await.unwrap();

        // Perform token refresh with custom expiration duration
        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: client_uuid.to_string(),
            refresh_token: raw_refresh.to_string(),
            use_cookie: Some(true),
            expires_in_secs: Some(30),
        };

        let response = refresh_handler(State(state.clone()), Json(payload))
            .await
            .expect("Refresh should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);

        // Verify Set-Cookie header contains Max-Age=30
        let cookie_header = response.headers().get(axum::http::header::SET_COOKIE).unwrap();
        let cookie_str = cookie_header.to_str().unwrap();
        assert!(cookie_str.contains("Max-Age=30"), "Set-Cookie should have Max-Age=30, got: {}", cookie_str);

        // Fetch session from DB and verify rotation columns
        let session = sqlx::query_as!(
            crate::auth::models::Session,
            "SELECT user_id, client_uuid, refresh_token_hash, expires_at, created_at, old_refresh_token_hash, rotated_at, failed_refresh_attempts FROM sessions WHERE user_id = $1 AND client_uuid = $2",
            user_id,
            client_uuid
        ).fetch_one(&pool).await.unwrap();

        // Verification: initial refresh token hash must now be saved under old_refresh_token_hash
        assert!(session.old_refresh_token_hash.is_some());
        assert!(verify_refresh_token(session.old_refresh_token_hash.as_ref().unwrap(), raw_refresh));
        assert!(session.rotated_at.is_some());
    }

    #[sqlx::test]
    async fn test_refresh_handler_grace_period(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-grace-test";
        let client_uuid = "client-grace-test";

        // Insert a session rotated a moment ago: the ordinary race, well inside the window.
        let old_refresh = "old-refresh-token-123";
        let current_refresh = "current-refresh-token-456";
        let old_hash = hash_refresh_token(old_refresh);
        let current_hash = hash_refresh_token(current_refresh);
        let expiration = chrono::Utc::now() + chrono::Duration::days(1);
        let rotated_at = chrono::Utc::now() - chrono::Duration::seconds(5);

        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
            user_id,
            client_uuid,
            current_hash,
            expiration,
            old_hash,
            rotated_at
        ).execute(&pool).await.unwrap();

        // Refresh with the OLD refresh token (simulating race/concurrency retry)
        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: client_uuid.to_string(),
            refresh_token: old_refresh.to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };

        let response = refresh_handler(State(state.clone()), Json(payload))
            .await
            .expect("Refresh with old token inside grace period should succeed");

        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }

    #[sqlx::test]
    async fn test_refresh_handler_breach_mitigation(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-breach-test";
        let client_uuid = "client-breach-test";
        let other_client_uuid = "other-client-breach-test";

        // Insert session rotated just outside the grace window
        let old_refresh = "old-refresh-token-123";
        let current_refresh = "current-refresh-token-456";
        let old_hash = hash_refresh_token(old_refresh);
        let current_hash = hash_refresh_token(current_refresh);
        let expiration = chrono::Utc::now() + chrono::Duration::days(1);
        let rotated_at = chrono::Utc::now()
            - chrono::Duration::seconds(REFRESH_GRACE_SECS + 5);

        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
            user_id,
            client_uuid,
            current_hash,
            expiration,
            old_hash,
            rotated_at
        ).execute(&pool).await.unwrap();

        // Insert a second session for the same user (active client)
        let active_refresh = "active-refresh-token-789";
        let active_hash = hash_refresh_token(active_refresh);
        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, NULL, NULL)",
            user_id,
            other_client_uuid,
            active_hash,
            expiration
        ).execute(&pool).await.unwrap();

        // Refresh with the old refresh token AFTER the grace period expires
        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: client_uuid.to_string(),
            refresh_token: old_refresh.to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };

        let response = refresh_handler(State(state.clone()), Json(payload)).await;
        assert_eq!(response.unwrap_err().status(), axum::http::StatusCode::UNAUTHORIZED);

        // Verify breach mitigation: only the offending client's session must be deleted
        let offending_exists = sqlx::query!("SELECT COUNT(*) as count FROM sessions WHERE user_id = $1 AND client_uuid = $2", user_id, client_uuid)
            .fetch_one(&pool)
            .await
            .unwrap()
            .count
            .unwrap() > 0;
        assert!(!offending_exists, "Offending client session should have been deleted");

        let other_exists = sqlx::query!("SELECT COUNT(*) as count FROM sessions WHERE user_id = $1 AND client_uuid = $2", user_id, other_client_uuid)
            .fetch_one(&pool)
            .await
            .unwrap()
            .count
            .unwrap() > 0;
        assert!(other_exists, "Other client session should still exist");
    }

    #[sqlx::test]
    async fn test_refresh_handler_expired_session_isolated(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-expired-isolated-test";
        let client_expired = "client-expired";
        let client_active = "client-active";

        // 1. Insert an EXPIRED session for device 1
        let refresh_expired = "expired-token-123";
        let hash_expired = hash_refresh_token(refresh_expired);
        let expiration_past = chrono::Utc::now() - chrono::Duration::seconds(10);
        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, NULL, NULL)",
            user_id,
            client_expired,
            hash_expired,
            expiration_past
        ).execute(&pool).await.unwrap();

        // 2. Insert an ACTIVE session for device 2
        let refresh_active = "active-token-456";
        let hash_active = hash_refresh_token(refresh_active);
        let expiration_future = chrono::Utc::now() + chrono::Duration::days(1);
        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, NULL, NULL)",
            user_id,
            client_active,
            hash_active,
            expiration_future
        ).execute(&pool).await.unwrap();

        // 3. Attempt to refresh the expired session
        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: client_expired.to_string(),
            refresh_token: refresh_expired.to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };

        let response = refresh_handler(State(state.clone()), Json(payload)).await;
        assert_eq!(response.unwrap_err().status(), axum::http::StatusCode::UNAUTHORIZED);

        // 4. Verify that the expired session was deleted
        let expired_exists = sqlx::query!(
            "SELECT COUNT(*) as count FROM sessions WHERE user_id = $1 AND client_uuid = $2",
            user_id,
            client_expired
        ).fetch_one(&pool).await.unwrap().count.unwrap() > 0;
        assert!(!expired_exists, "Expired session should be deleted");

        // 5. Verify that the active session was NOT deleted (isolated deletion)
        let active_exists = sqlx::query!(
            "SELECT COUNT(*) as count FROM sessions WHERE user_id = $1 AND client_uuid = $2",
            user_id,
            client_active
        ).fetch_one(&pool).await.unwrap().count.unwrap() > 0;
        assert!(active_exists, "Active session should still exist");
    }

    /// A refresh token that was never issued proves only that the caller is guessing, and
    /// guessing must not log a device out: `/auth/refresh` takes no credential beyond the
    /// token itself, so deleting here made a one-line unauthenticated POST into a permanent
    /// remote logout for any `user_id`/`client_uuid` an attacker could name.
    #[sqlx::test]
    async fn test_refresh_handler_unknown_token_leaves_session_intact(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-invalid-token-test";
        let client_1 = "client-1";
        let client_2 = "client-2";

        // Insert session 1
        let refresh_1 = "token-1";
        let hash_1 = hash_refresh_token(refresh_1);
        let expiration = chrono::Utc::now() + chrono::Duration::days(1);
        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, NULL, NULL)",
            user_id,
            client_1,
            hash_1,
            expiration
        ).execute(&pool).await.unwrap();

        // Insert session 2
        let refresh_2 = "token-2";
        let hash_2 = hash_refresh_token(refresh_2);
        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, NULL, NULL)",
            user_id,
            client_2,
            hash_2,
            expiration
        ).execute(&pool).await.unwrap();

        // Attempt refresh on session 1 with a completely invalid token, three times over.
        for _ in 0..3 {
            let payload = RefreshRequest {
                user_id: user_id.to_string(),
                client_uuid: client_1.to_string(),
                refresh_token: "wrong-token-abc".to_string(),
                use_cookie: Some(false),
                expires_in_secs: None,
            };
            let response = refresh_handler(State(state.clone()), Json(payload)).await;
            assert_eq!(response.unwrap_err().status(), axum::http::StatusCode::UNAUTHORIZED);
        }

        // Neither session may be touched by an unauthenticated guess.
        for client in [client_1, client_2] {
            let exists = sqlx::query!("SELECT COUNT(*) as count FROM sessions WHERE user_id = $1 AND client_uuid = $2", user_id, client)
                .fetch_one(&pool)
                .await
                .unwrap()
                .count
                .unwrap() > 0;
            assert!(exists, "Session for {} must survive a guessed refresh token", client);
        }

        // The guesses are counted, so a brute-force is still visible.
        let attempts = sqlx::query_scalar!(
            "SELECT failed_refresh_attempts FROM sessions WHERE user_id = $1 AND client_uuid = $2",
            user_id,
            client_1
        ).fetch_one(&pool).await.unwrap();
        assert_eq!(attempts, 3, "Each rejected guess should bump the per-session counter");

        // And the device whose session was targeted can still refresh normally afterwards.
        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: client_1.to_string(),
            refresh_token: refresh_1.to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };
        let response = refresh_handler(State(state.clone()), Json(payload))
            .await
            .expect("The real refresh token must still work after someone guessed at it");
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        // A successful rotation clears the counter: it counts consecutive failures.
        let attempts = sqlx::query_scalar!(
            "SELECT failed_refresh_attempts FROM sessions WHERE user_id = $1 AND client_uuid = $2",
            user_id,
            client_1
        ).fetch_one(&pool).await.unwrap();
        assert_eq!(attempts, 0, "A successful refresh should reset the failure counter");
    }

    /// The old hash matching with a NULL `rotated_at` cannot be reached by guessing -- only a
    /// genuinely issued token matches it -- so invalidating there is not the remote-logout
    /// hole, and we keep failing closed rather than honouring a token of unknowable age.
    #[sqlx::test]
    async fn test_refresh_handler_rotated_at_null_invalidates(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-rotated-null-test";
        let client_uuid = "client-rotated-null";

        let old_refresh = "old-refresh-token-null-rotated";
        let current_refresh = "current-refresh-token-null-rotated";
        let expiration = chrono::Utc::now() + chrono::Duration::days(1);
        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, $5, NULL)",
            user_id,
            client_uuid,
            hash_refresh_token(current_refresh),
            expiration,
            hash_refresh_token(old_refresh)
        ).execute(&pool).await.unwrap();

        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: client_uuid.to_string(),
            refresh_token: old_refresh.to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };
        let response = refresh_handler(State(state.clone()), Json(payload)).await;
        assert_eq!(response.unwrap_err().status(), axum::http::StatusCode::UNAUTHORIZED);

        let exists = sqlx::query!("SELECT COUNT(*) as count FROM sessions WHERE user_id = $1 AND client_uuid = $2", user_id, client_uuid)
            .fetch_one(&pool)
            .await
            .unwrap()
            .count
            .unwrap() > 0;
        assert!(!exists, "A real old token of unknowable age should still invalidate the session");
    }

    /// The exhaustive list of diagnostics that used to ride along on a `refresh_handler`
    /// failure and must never appear in a response body again. `active_clients` was the
    /// serious one — an unauthenticated caller who knew a `user_id` got back every
    /// `client_uuid` on that account — but the timestamps, token lengths and
    /// debug-formatted database errors were schema and state disclosure on the same
    /// anonymous path, so they go too. All of it is still in the `tracing` output.
    const FORBIDDEN_IN_REFRESH_ERRORS: &[&str] = &[
        "active_clients",
        "details",
        "message",
        "expires_at",
        "rotated_at",
        "server_time",
        "age_seconds",
        "provided_token_length",
        "has_old_refresh_token_hash",
        "db_error",
        "user_id",
        "client_uuid",
        "Database",
        "sessions",
    ];

    /// Asserts a `refresh_handler` failure response is the whole contract from
    /// `refresh_error`: the expected status, a body of exactly `{"error": <code>}` with
    /// no second key, and no trace of any removed diagnostic anywhere in the raw bytes.
    ///
    /// `must_not_appear` carries the case-specific needles — typically the `client_uuid`
    /// of a *sibling* session on the same account, which is exactly what the old
    /// `active_clients` query would have handed back.
    async fn assert_minimal_refresh_error(
        response: axum::response::Response,
        expected_status: axum::http::StatusCode,
        expected_code: &str,
        must_not_appear: &[&str],
    ) {
        assert_eq!(response.status(), expected_status);

        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("failure body should be readable");
        let raw = String::from_utf8(bytes.to_vec()).expect("failure body should be UTF-8");

        let json: serde_json::Value =
            serde_json::from_str(&raw).unwrap_or_else(|e| panic!("body {raw:?} is not JSON: {e}"));
        let object = json.as_object().expect("failure body should be a JSON object");

        assert_eq!(
            object.len(),
            1,
            "failure body must carry the error code and nothing else, got {raw}"
        );
        assert_eq!(object.get("error").and_then(|v| v.as_str()), Some(expected_code));

        for needle in FORBIDDEN_IN_REFRESH_ERRORS {
            assert!(
                !raw.contains(needle),
                "failure body leaked {needle:?}: {raw}"
            );
        }
        for needle in must_not_appear {
            assert!(
                !raw.contains(needle),
                "failure body leaked {needle:?}: {raw}"
            );
        }
    }

    /// Inserts a second, healthy session on the same account. Its `client_uuid` is the
    /// thing the old `details.active_clients` list disclosed, so every failure-body test
    /// below plants one and then asserts it does not come back.
    async fn insert_sibling_session(pool: &PgPool, user_id: &str, client_uuid: &str) {
        let hash = hash_refresh_token("sibling-refresh-token");
        let expiration = chrono::Utc::now() + chrono::Duration::days(1);
        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, NULL, NULL)",
            user_id,
            client_uuid,
            hash,
            expiration
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[sqlx::test]
    async fn test_refresh_error_body_session_not_found(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-body-not-found";
        let sibling = "sibling-device-not-found";
        insert_sibling_session(&pool, user_id, sibling).await;

        // No session exists for this client_uuid: the attacker's invented device.
        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: "device-that-does-not-exist".to_string(),
            refresh_token: "anything".to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };

        let response = refresh_handler(State(state), Json(payload)).await.unwrap_err();
        assert_minimal_refresh_error(
            response,
            axum::http::StatusCode::UNAUTHORIZED,
            "unauthorized",
            &[sibling],
        )
        .await;
    }

    #[sqlx::test]
    async fn test_refresh_error_body_session_expired(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-body-expired";
        let client_uuid = "client-body-expired";
        let sibling = "sibling-device-expired";
        insert_sibling_session(&pool, user_id, sibling).await;

        let raw_refresh = "expired-but-current-token";
        let hash = hash_refresh_token(raw_refresh);
        let expiration_past = chrono::Utc::now() - chrono::Duration::seconds(10);
        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, NULL, NULL)",
            user_id,
            client_uuid,
            hash,
            expiration_past
        )
        .execute(&pool)
        .await
        .unwrap();

        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: client_uuid.to_string(),
            refresh_token: raw_refresh.to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };

        let response = refresh_handler(State(state), Json(payload)).await.unwrap_err();
        assert_minimal_refresh_error(
            response,
            axum::http::StatusCode::UNAUTHORIZED,
            "unauthorized",
            &[sibling],
        )
        .await;
    }

    #[sqlx::test]
    async fn test_refresh_error_body_grace_period_expired(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-body-grace";
        let client_uuid = "client-body-grace";
        let sibling = "sibling-device-grace";
        insert_sibling_session(&pool, user_id, sibling).await;

        let old_refresh = "old-token-outside-grace";
        let old_hash = hash_refresh_token(old_refresh);
        let current_hash = hash_refresh_token("current-token");
        let expiration = chrono::Utc::now() + chrono::Duration::days(1);
        let rotated_at = chrono::Utc::now()
            - chrono::Duration::seconds(REFRESH_GRACE_SECS + 5);
        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
            user_id,
            client_uuid,
            current_hash,
            expiration,
            old_hash,
            rotated_at
        )
        .execute(&pool)
        .await
        .unwrap();

        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: client_uuid.to_string(),
            refresh_token: old_refresh.to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };

        let response = refresh_handler(State(state), Json(payload)).await.unwrap_err();
        assert_minimal_refresh_error(
            response,
            axum::http::StatusCode::UNAUTHORIZED,
            "unauthorized",
            &[sibling],
        )
        .await;
    }

    #[sqlx::test]
    async fn test_refresh_error_body_session_expired_during_grace_period(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-body-expired-grace";
        let client_uuid = "client-body-expired-grace";
        let sibling = "sibling-device-expired-grace";
        insert_sibling_session(&pool, user_id, sibling).await;

        // Old token presented well inside the 30s grace window, but the session itself
        // has already expired.
        let old_refresh = "old-token-inside-grace";
        let old_hash = hash_refresh_token(old_refresh);
        let current_hash = hash_refresh_token("current-token");
        let expiration_past = chrono::Utc::now() - chrono::Duration::seconds(10);
        let rotated_at = chrono::Utc::now() - chrono::Duration::seconds(5);
        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
            user_id,
            client_uuid,
            current_hash,
            expiration_past,
            old_hash,
            rotated_at
        )
        .execute(&pool)
        .await
        .unwrap();

        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: client_uuid.to_string(),
            refresh_token: old_refresh.to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };

        let response = refresh_handler(State(state), Json(payload)).await.unwrap_err();
        assert_minimal_refresh_error(
            response,
            axum::http::StatusCode::UNAUTHORIZED,
            "unauthorized",
            &[sibling],
        )
        .await;
    }

    #[sqlx::test]
    async fn test_refresh_error_body_rotated_at_null(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-body-rotated-null";
        let client_uuid = "client-body-rotated-null";
        let sibling = "sibling-device-rotated-null";
        insert_sibling_session(&pool, user_id, sibling).await;

        let old_refresh = "old-token-no-rotated-at";
        let old_hash = hash_refresh_token(old_refresh);
        let current_hash = hash_refresh_token("current-token");
        let expiration = chrono::Utc::now() + chrono::Duration::days(1);
        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, $5, NULL)",
            user_id,
            client_uuid,
            current_hash,
            expiration,
            old_hash
        )
        .execute(&pool)
        .await
        .unwrap();

        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: client_uuid.to_string(),
            refresh_token: old_refresh.to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };

        let response = refresh_handler(State(state), Json(payload)).await.unwrap_err();
        assert_minimal_refresh_error(
            response,
            axum::http::StatusCode::UNAUTHORIZED,
            "unauthorized",
            &[sibling],
        )
        .await;
    }

    #[sqlx::test]
    async fn test_refresh_error_body_token_mismatch(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-body-mismatch";
        let client_uuid = "client-body-mismatch";
        let sibling = "sibling-device-mismatch";
        insert_sibling_session(&pool, user_id, sibling).await;

        let hash = hash_refresh_token("the-real-token");
        let expiration = chrono::Utc::now() + chrono::Duration::days(1);
        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, NULL, NULL)",
            user_id,
            client_uuid,
            hash,
            expiration
        )
        .execute(&pool)
        .await
        .unwrap();

        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: client_uuid.to_string(),
            // A long wrong token: the old body echoed `provided_token_length`.
            refresh_token: "a-completely-wrong-token-of-a-distinctive-length".to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };

        let response = refresh_handler(State(state), Json(payload)).await.unwrap_err();
        assert_minimal_refresh_error(
            response,
            axum::http::StatusCode::UNAUTHORIZED,
            "unauthorized",
            &[sibling],
        )
        .await;
    }

    /// The regression this whole change exists to prevent.
    ///
    /// `COOKIE_DOMAIN` used to be the gate on the `mock.` bypass, so setting it to the empty
    /// string — a legitimate configuration meaning "no Domain attribute on the cookie", which
    /// is what a single-host deployment wants — silently turned `POST /auth/login` into
    /// "mint a session for any `user_id` you can name". Cookie configuration must have no
    /// bearing on authentication, so both spellings are asserted here: in a build without
    /// `dev-auth` a `mock.` token is rejected either way, and nothing is written.
    ///
    /// (This drives the handler rather than `dev_bypass_identity` directly, so it covers the
    /// wiring too. Rejection happens inside `validate_id_token`, which cannot parse
    /// `mock.token` as a JWT and so fails before it would reach out to Google's certs.)
    #[cfg(not(feature = "dev-auth"))]
    #[sqlx::test]
    async fn test_mock_token_is_rejected_whatever_the_cookie_domain(pool: PgPool) {
        for cookie_domain in ["", ".teddy.fyi"] {
            let mut state = setup_state(pool.clone());
            state.cookie_domain = cookie_domain.to_string();

            let payload = LoginRequest {
                user_id: "victim-user-id".to_string(),
                client_uuid: "attacker-client".to_string(),
                google_auth_token: "mock.token".to_string(),
                use_cookie: Some(false),
                expires_in_secs: None,
            };

            let status = login_handler(State(state), Json(payload))
                .await
                .expect_err("a `mock.` token must not authenticate in a production build");
            assert_eq!(
                status,
                axum::http::StatusCode::UNAUTHORIZED,
                "COOKIE_DOMAIN={cookie_domain:?} must not change this"
            );
        }

        // Nothing was minted on the way to those rejections. These use the runtime query
        // API rather than `sqlx::query!` on purpose: a macro query inside a `cfg`-gated
        // test is only visible to `cargo sqlx prepare` in the configuration it compiles in,
        // so it would make the checked-in `.sqlx` cache correct for one build and wrong for
        // the other.
        let user_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE id = $1")
            .bind("victim-user-id")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(user_rows, 0, "a rejected login must not upsert a user");

        let session_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = $1")
                .bind("victim-user-id")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(session_rows, 0, "a rejected login must not create a session");
    }

    /// The other half of the same property, in the build that *does* carry the bypass: a
    /// `mock.` token works, and it works with the production-shaped `COOKIE_DOMAIN` set,
    /// proving the two are no longer coupled in either direction.
    #[cfg(feature = "dev-auth")]
    #[sqlx::test]
    async fn test_mock_token_is_accepted_with_a_non_empty_cookie_domain(pool: PgPool) {
        let mut state = setup_state(pool.clone());
        state.cookie_domain = ".teddy.fyi".to_string();

        let payload = LoginRequest {
            user_id: "dev-user-id".to_string(),
            client_uuid: "dev-client".to_string(),
            google_auth_token: "mock.token".to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };

        let response = login_handler(State(state), Json(payload))
            .await
            .expect("a dev-auth build must still accept `mock.` tokens");
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        // Runtime query rather than the macro, for the `.sqlx`-cache reason noted on the
        // sibling test above.
        let email: Option<String> = sqlx::query_scalar("SELECT email FROM users WHERE id = $1")
            .bind("dev-user-id")
            .fetch_one(&pool)
            .await
            .expect("the dev login should have upserted the user");
        assert_eq!(
            email.as_deref(),
            Some(crate::auth::dev_bypass::DEV_USER_EMAIL)
        );
    }

    /// An empty `COOKIE_DOMAIN` still has to do its actual job — omit the `Domain`
    /// attribute — in both build configurations, since that is the setting the old gate
    /// made unsafe to choose.
    #[test]
    fn test_session_cookie_shape_with_and_without_a_domain() {
        let with_domain = crate::auth::handlers::session_cookie(".teddy.fyi", "tok", 10);
        assert_eq!(
            with_domain,
            "access_token=tok; HttpOnly; Secure; SameSite=Lax; Domain=.teddy.fyi; Path=/; Max-Age=10"
        );

        let host_only = crate::auth::handlers::session_cookie("", "tok", 10);
        assert_eq!(
            host_only,
            "access_token=tok; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=10"
        );
        assert!(!host_only.contains("Domain"));

        // The logout spelling: same attributes, empty value, immediate expiry.
        assert_eq!(
            crate::auth::handlers::session_cookie("", "", 0),
            "access_token=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0"
        );
        assert_eq!(
            crate::auth::handlers::session_cookie(".teddy.fyi", "", 0),
            "access_token=; HttpOnly; Secure; SameSite=Lax; Domain=.teddy.fyi; Path=/; Max-Age=0"
        );
    }

    /// What "sign out" actually costs, stated as a test.
    ///
    /// Logging out deletes the session row, which ends *refresh* immediately, and clears the
    /// cookie. It cannot end the bearer token already in someone's hand: `require_auth` does
    /// no session lookup, by design. So the honest claim is a bounded one — the token keeps
    /// working for at most one access-token lifetime — and that bound is what this pins. When
    /// it was 24 hours the same test would have passed while describing something nobody
    /// would call signing out.
    /// Signs a token for `user_id`/`client_uuid` that expired `ago_secs` ago.
    ///
    /// Minted here rather than through `create_access_token`, which clamps every request to
    /// [`ACCESS_TOKEN_TTL_SECS`] and so cannot produce one that is already dead. The signature
    /// is real; only the clock is in the past.
    fn expired_token(secret: &str, user_id: &str, client_uuid: &str, ago_secs: i64) -> String {
        let claims = Claims {
            sub: user_id.to_string(),
            client_uuid: client_uuid.to_string(),
            exp: (chrono::Utc::now().timestamp() - ago_secs) as usize,
        };
        jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .expect("encoding a test token should succeed")
    }

    fn bearer(token: &str) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    /// The defect this pins: signing out with an already-expired access token used to clear the
    /// cookie and leave the session row alive, so the refresh token in it stayed good for its
    /// remaining seven days. At a 15-minute TTL that is not a corner case -- it is what a parent
    /// presents whenever the app has been open longer than a quarter of an hour.
    #[sqlx::test]
    async fn test_logout_with_an_expired_token_still_ends_the_session(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-logout-expired";
        let client_uuid = "client-logout-expired";

        crate::auth::handlers::issue_session(
            &state,
            user_id,
            None,
            client_uuid,
            DEFAULT_SESSION_SECS,
        )
        .await
        .expect("issuing a session should succeed");

        // An hour dead: far outside any leeway jsonwebtoken applies by default.
        let stale = expired_token(&state.jwt_secret, user_id, client_uuid, 3600);

        let response = logout_handler(State(state.clone()), bearer(&stale))
            .await
            .expect("logout should succeed");
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let sessions = sqlx::query!(
            "SELECT COUNT(*) as count FROM sessions WHERE user_id = $1 AND client_uuid = $2",
            user_id,
            client_uuid
        )
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
        assert_eq!(
            sessions, 0,
            "an expired access token must still be able to end its own session"
        );
    }

    /// The guard that makes relaxing `exp` safe: the *signature* is what still has to hold.
    ///
    /// Without this, `logout_validation` would be one step from an anonymous remote-logout hole
    /// -- name any `user_id`/`client_uuid` and end that session. The claims are not
    /// attacker-chosen, because a token signed with any other key does not decode at all.
    #[sqlx::test]
    async fn test_logout_cannot_be_forged_for_another_user(pool: PgPool) {
        let state = setup_state(pool.clone());
        let victim = "user-logout-victim";
        let victim_client = "client-logout-victim";

        crate::auth::handlers::issue_session(
            &state,
            victim,
            None,
            victim_client,
            DEFAULT_SESSION_SECS,
        )
        .await
        .expect("issuing a session should succeed");

        // Correctly shaped, correctly named, signed with a key this service never issued.
        let forged = expired_token("not-the-servers-secret", victim, victim_client, 3600);

        let response = logout_handler(State(state.clone()), bearer(&forged))
            .await
            .expect("logout answers OK either way; what matters is the row");
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let sessions = sqlx::query!(
            "SELECT COUNT(*) as count FROM sessions WHERE user_id = $1 AND client_uuid = $2",
            victim,
            victim_client
        )
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
        assert_eq!(
            sessions, 1,
            "a token this service did not sign must not end anyone's session"
        );
    }

    #[sqlx::test]
    async fn test_logout_ends_refresh_and_bounds_the_access_token_to_one_ttl(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-logout-window";
        let client_uuid = "client-logout-window";

        let issued = crate::auth::handlers::issue_session(
            &state,
            user_id,
            None,
            client_uuid,
            DEFAULT_SESSION_SECS,
        )
        .await
        .expect("issuing a session should succeed");

        let decode = |token: &str| {
            jsonwebtoken::decode::<Claims>(
                token,
                &jsonwebtoken::DecodingKey::from_secret(state.jwt_secret.as_bytes()),
                &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
            )
        };

        let before = decode(&issued.access_token).expect("freshly minted token is valid");
        let remaining = before.claims.exp as i64 - chrono::Utc::now().timestamp();
        assert!(
            remaining <= ACCESS_TOKEN_TTL_SECS,
            "a minted token must never outlive the TTL; {remaining}s remained"
        );

        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {}", issued.access_token).parse().unwrap(),
        );
        let response = logout_handler(State(state.clone()), headers)
            .await
            .expect("logout should succeed");
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        // Refresh is dead the instant logout returns: the row is gone.
        let sessions = sqlx::query!(
            "SELECT COUNT(*) as count FROM sessions WHERE user_id = $1 AND client_uuid = $2",
            user_id,
            client_uuid
        )
        .fetch_one(&pool)
        .await
        .unwrap()
        .count
        .unwrap();
        assert_eq!(sessions, 0, "logout must delete the session row");

        let refused = refresh_handler(
            State(state.clone()),
            Json(RefreshRequest {
                user_id: user_id.to_string(),
                client_uuid: client_uuid.to_string(),
                refresh_token: issued.refresh_token.clone(),
                use_cookie: Some(false),
                expires_in_secs: None,
            }),
        )
        .await;
        assert_eq!(
            refused.unwrap_err().status(),
            axum::http::StatusCode::UNAUTHORIZED,
            "a logged-out session must not be refreshable"
        );

        // The access token, meanwhile, still verifies — this is the residual exposure the
        // short TTL exists to bound, and it is deliberately asserted rather than wished away.
        let after = decode(&issued.access_token)
            .expect("the bearer token survives logout; only its short life ends it");
        let residual = after.claims.exp as i64 - chrono::Utc::now().timestamp();
        assert!(
            residual <= ACCESS_TOKEN_TTL_SECS,
            "post-logout exposure must be bounded by the TTL; {residual}s remained"
        );
    }

    /// The widened rotation grace window, pinned at both edges.
    ///
    /// The lower assertion is the one that matters: 30 seconds was the old window, and a
    /// client that now refreshes ~96 times a day instead of once meets every way of losing a
    /// rotation response ~100x more often. A retry at 30s must still be a retry, not a
    /// "breach" that deletes the session out from under a parent.
    /// The test below only proves something if the window is wider than the old 30 seconds,
    /// so that premise is checked at compile time rather than left to a reader.
    const _: () = assert!(REFRESH_GRACE_SECS - 5 > 30);

    #[sqlx::test]
    async fn test_refresh_grace_window_covers_a_slow_retry(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-grace-edge";
        let client_uuid = "client-grace-edge";

        let old_refresh = "old-token-at-the-edge";
        let old_hash = hash_refresh_token(old_refresh);
        let current_hash = hash_refresh_token("current-token-at-the-edge");
        let expiration = chrono::Utc::now() + chrono::Duration::days(1);
        // Five seconds inside the window, which is also well outside the old 30s one.
        let rotated_at =
            chrono::Utc::now() - chrono::Duration::seconds(REFRESH_GRACE_SECS - 5);

        sqlx::query!(
            "INSERT INTO sessions (user_id, client_uuid, refresh_token_hash, expires_at, old_refresh_token_hash, rotated_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
            user_id,
            client_uuid,
            current_hash,
            expiration,
            old_hash,
            rotated_at
        )
        .execute(&pool)
        .await
        .unwrap();

        let response = refresh_handler(
            State(state.clone()),
            Json(RefreshRequest {
                user_id: user_id.to_string(),
                client_uuid: client_uuid.to_string(),
                refresh_token: old_refresh.to_string(),
                use_cookie: Some(false),
                expires_in_secs: None,
            }),
        )
        .await
        .expect("a retry inside the grace window must succeed, not end the session");
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        // And the session survives: the racer's token became the stored `old` one, so
        // whichever caller won the first rotation still holds a token that works.
        let session = sqlx::query!(
            "SELECT old_refresh_token_hash FROM sessions WHERE user_id = $1 AND client_uuid = $2",
            user_id,
            client_uuid
        )
        .fetch_one(&pool)
        .await
        .expect("the session must still exist");
        assert!(session.old_refresh_token_hash.is_some());
    }

    /// Refreshing needs no access token at all — the whole credential is in the body.
    ///
    /// This is what makes the short TTL a *replacement* for a `kid` header rather than a
    /// regression: after a `JWT_SECRET` rotation every outstanding access token is
    /// unverifiable, but the recovery path does not check one, so a client takes a single 401
    /// and refreshes back into a working session instead of being signed out.
    #[sqlx::test]
    async fn test_refresh_needs_no_access_token(pool: PgPool) {
        let state = setup_state(pool.clone());
        let user_id = "user-refresh-no-access-token";
        let client_uuid = "client-refresh-no-access-token";

        let issued = crate::auth::handlers::issue_session(
            &state,
            user_id,
            None,
            client_uuid,
            DEFAULT_SESSION_SECS,
        )
        .await
        .expect("issuing a session should succeed");

        // Note what is *not* here: `RefreshRequest` has nowhere to put an access token, and
        // the handler takes no `HeaderMap`. A token signed with a retired secret, or an
        // expired one, cannot fail this call because it is never presented.
        let response = refresh_handler(
            State(state.clone()),
            Json(RefreshRequest {
                user_id: user_id.to_string(),
                client_uuid: client_uuid.to_string(),
                refresh_token: issued.refresh_token,
                use_cookie: Some(false),
                expires_in_secs: None,
            }),
        )
        .await
        .expect("refresh must work without any access token");
        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }
}
