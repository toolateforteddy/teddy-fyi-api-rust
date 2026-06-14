#[cfg(test)]
mod tests {
    use crate::auth::tokens::{create_access_token, hash_refresh_token, verify_refresh_token};

    #[test]
    fn test_token_lifecycle() {
        let secret = b"super-secret-key-for-testing";
        let user_id = "user123";
        let client_uuid = "device-abc";

        // Test JWT creation
        let token = create_access_token(user_id, client_uuid, secret).unwrap();
        assert!(!token.is_empty());

        // Test Refresh token hashing
        let raw_refresh = "very-secret-refresh-token";
        let hash = hash_refresh_token(raw_refresh);

        assert!(verify_refresh_token(&hash, raw_refresh));
        assert!(!verify_refresh_token(&hash, "wrong-token"));
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
}
