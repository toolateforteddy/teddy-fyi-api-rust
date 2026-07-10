#[cfg(test)]
mod tests {
    use crate::auth::tokens::{create_access_token, hash_refresh_token, verify_refresh_token, Claims};
    use crate::auth::handlers::{login_handler, refresh_handler, LoginRequest, RefreshRequest};
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

        // 2. Test ceiling limit (exceeding 24 hours caps to 24 hours)
        let token_large = create_access_token(user_id, client_uuid, secret, Some(200000)).unwrap();
        let decoded_large = jsonwebtoken::decode::<Claims>(
            &token_large,
            &jsonwebtoken::DecodingKey::from_secret(secret),
            &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
        ).unwrap();
        let diff_large = decoded_large.claims.exp - now;
        assert!(diff_large >= 86390 && diff_large <= 86410);

        // 3. Test negative duration defaults to 24 hours
        let token_neg = create_access_token(user_id, client_uuid, secret, Some(-100)).unwrap();
        let decoded_neg = jsonwebtoken::decode::<Claims>(
            &token_neg,
            &jsonwebtoken::DecodingKey::from_secret(secret),
            &jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::HS256),
        ).unwrap();
        let diff_neg = decoded_neg.claims.exp - now;
        assert!(diff_neg >= 86390 && diff_neg <= 86410);
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

    #[sqlx::test]
    async fn test_login_handler_custom_duration(pool: PgPool) {
        let mut state = setup_state(pool.clone());
        state.cookie_domain = "".to_string(); // bypass Google OAuth validation via dev/mock token

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
            "SELECT * FROM sessions WHERE user_id = $1 AND client_uuid = $2",
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

        // Insert rotated session (rotated 5 seconds ago)
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

        // Insert session rotated 35 seconds ago (outside grace period)
        let old_refresh = "old-refresh-token-123";
        let current_refresh = "current-refresh-token-456";
        let old_hash = hash_refresh_token(old_refresh);
        let current_hash = hash_refresh_token(current_refresh);
        let expiration = chrono::Utc::now() + chrono::Duration::days(1);
        let rotated_at = chrono::Utc::now() - chrono::Duration::seconds(35);

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

        // Refresh with the old refresh token AFTER the grace period expires
        let payload = RefreshRequest {
            user_id: user_id.to_string(),
            client_uuid: client_uuid.to_string(),
            refresh_token: old_refresh.to_string(),
            use_cookie: Some(false),
            expires_in_secs: None,
        };

        let response = refresh_handler(State(state.clone()), Json(payload)).await;
        assert_eq!(response.unwrap_err(), axum::http::StatusCode::UNAUTHORIZED);

        // Verify breach mitigation: all sessions for the user must be deleted from the database
        let count = sqlx::query!("SELECT COUNT(*) as count FROM sessions WHERE user_id = $1", user_id)
            .fetch_one(&pool)
            .await
            .unwrap()
            .count
            .unwrap();
        assert_eq!(count, 0, "All sessions for user should have been deleted");
    }
}
