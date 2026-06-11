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
}
