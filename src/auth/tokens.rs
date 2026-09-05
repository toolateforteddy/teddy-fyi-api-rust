use jsonwebtoken::{encode, Header, EncodingKey};
use serde::{Deserialize, Serialize};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};

/// How long an access token is good for, and the hard ceiling on any lifetime a caller may
/// ask for. Fifteen minutes.
///
/// This number *is* the revocation story. [`crate::auth::middleware::require_auth`] checks the
/// signature, the `exp` and the `X-Client-UUID` binding and nothing else — there is
/// deliberately no session lookup on the authenticated path, because that would put a query on
/// every request against a small pool. The consequence is that nothing can retract a token
/// that has already been minted: `logout_handler` deletes the `sessions` row,
/// `DELETE /api/user/data` deletes the account and `jobs::reap_stale_users` reaps it, and in
/// all three cases a bearer token already in someone's hands keeps authenticating until it
/// expires on its own.
///
/// So the window a parent accepts when they tap "sign out" on a tablet that has left the house
/// is exactly this constant. It used to be 24 hours, which is not a window anyone would accept
/// if it were printed next to the button. Fifteen minutes is short enough to be honest about —
/// the lost device stops working while the parent is still holding the phone they signed out
/// from — and long enough that a device refreshes on the order of a hundred times a day rather
/// than tens of thousands: each refresh costs an argon2 verification and a row-locking
/// transaction, so this is also the knob that keeps `/auth/refresh` from becoming the busiest
/// endpoint in the service.
///
/// Nothing about a *tablet* argues for a longer value. The refresh token is the credential
/// with the long life (seven days, extended on every use) and it is stored on the device, so a
/// tablet that has been offline for a week comes back, gets one 401, refreshes, and carries on.
/// A short access token costs an offline device nothing, because an offline device is not
/// making requests.
///
/// [`crate::auth::handlers::DEFAULT_SESSION_SECS`] is defined as this constant, so the default
/// and the clamp cannot drift apart — a clamp above the value the mint enforces would be a lie,
/// which is what the two independent 24-hour numbers here previously were.
pub const ACCESS_TOKEN_TTL_SECS: i64 = 900;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String, // user_id
    pub client_uuid: String,
    pub exp: usize,
}

/// Mints an access token for `user_id`/`client_uuid`.
///
/// `expires_in_secs` is a *request*, not an instruction: anything longer than
/// [`ACCESS_TOKEN_TTL_SECS`], and anything non-positive, is replaced by that ceiling. A caller
/// can only ever ask for a shorter life than the server's maximum, so no combination of body
/// fields can widen the post-sign-out exposure window.
pub fn create_access_token(
    user_id: &str,
    client_uuid: &str,
    secret: &[u8],
    expires_in_secs: Option<i64>,
) -> Result<String, jsonwebtoken::errors::Error> {
    let max_duration = chrono::Duration::seconds(ACCESS_TOKEN_TTL_SECS);
    let duration = if let Some(secs) = expires_in_secs {
        let requested = chrono::Duration::seconds(secs);
        if requested > max_duration || requested <= chrono::Duration::zero() {
            max_duration
        } else {
            requested
        }
    } else {
        max_duration
    };

    let expiration = chrono::Utc::now()
        .checked_add_signed(duration)
        .expect("valid timestamp")
        .timestamp() as usize;

    let claims = Claims {
        sub: user_id.to_owned(),
        client_uuid: client_uuid.to_owned(),
        exp: expiration,
    };

    encode(&Header::default(), &claims, &EncodingKey::from_secret(secret))
}

pub fn hash_refresh_token(token: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2.hash_password(token.as_bytes(), &salt)
        .expect("failed to hash password")
        .to_string()
}

pub fn verify_refresh_token(hash: &str, token: &str) -> bool {
    let parsed_hash = argon2::PasswordHash::new(hash).expect("invalid hash format");
    Argon2::default().verify_password(token.as_bytes(), &parsed_hash).is_ok()
}
