use jsonwebtoken::{encode, decode, Header, Algorithm, Validation, EncodingKey, DecodingKey};
use serde::{Deserialize, Serialize};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String, // user_id
    pub client_uuid: String,
    pub exp: usize,
}

pub fn create_access_token(user_id: &str, client_uuid: &str, secret: &[u8]) -> Result<String, jsonwebtoken::errors::Error> {
    let expiration = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::hours(24))
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
