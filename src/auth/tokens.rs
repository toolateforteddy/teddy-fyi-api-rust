use crate::auth::product::Product;
use jsonwebtoken::{encode, Header, EncodingKey};
use serde::{Deserialize, Serialize};
use argon2::{password_hash::PasswordVerifier, Argon2};
use sha2::{Digest, Sha256};

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
    /// Which product this token was issued for, when that is known.
    ///
    /// This is the only thing in a token that distinguishes a ScribbleRoute credential
    /// from a teddy.fyi one. Everything else — the signature, the `exp`, the device
    /// binding — is identical across the two products, and `JWT_SECRET` is deliberately
    /// not rotated at the split, so without this claim a token minted at one hostname
    /// stays structurally valid at the other for as long as both exist.
    /// [`crate::routes::sync::scope_auth`] is what acts on it.
    ///
    /// `Option`, and skipped when absent, for the reason set out in
    /// [`crate::auth::product`]: tokens already in devices' hands do not carry it, sessions
    /// established through an unclassified client ID cannot carry it, and neither may be
    /// signed out by this change. A missing claim means "unknown", never "denied".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product: Option<Product>,
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
    product: Option<Product>,
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
        product,
    };

    encode(&Header::default(), &claims, &EncodingKey::from_secret(secret))
}

/// Prefix marking a stored hash as Argon2's PHC string format, which is what every
/// `sessions` row written before this change holds.
const ARGON2_PHC_PREFIX: &str = "$argon2";

/// Hashes a refresh token for storage: a domain-separated SHA-256, hex encoded.
///
/// This was Argon2id at default parameters (~19 MiB, tens of milliseconds) until it wasn't.
/// A refresh token is 64 characters of CSPRNG `Alphanumeric` — 380 bits of entropy, no
/// guessable structure, no low-entropy human-chosen part. A memory-hard KDF exists to make
/// *guessing* expensive, and there is nothing here to guess: an attacker who can brute
/// force this has already brute forced the token space itself. What the hashing requirement
/// actually buys is that a database dump yields no usable credential, and SHA-256 over 380
/// bits of randomness buys that just as completely.
///
/// What Argon2 cost, against a 256Mi pod, on the busiest authenticated path in the service:
///
/// * `/auth/refresh` paid up to **three** Argon2 operations per call — verify the current
///   hash, verify the old one, hash the new one. `ACCESS_TOKEN_TTL_SECS` is 900, so a
///   device hits that endpoint on the order of a hundred times a day, and ~19 MiB of
///   scratch per operation was the largest single allocation in the request path.
/// * `verify_refresh_token` reached it through `PasswordHash::new(hash).expect(...)`, a
///   panic on any row whose stored format was not parseable. `crate::guardrails` cites that
///   `.expect` by name as the live example of why `CatchPanicLayer` is there. A
///   deterministic hash removes the panic rather than catching it.
///
/// [`crate::auth::device::hash_device_code`] made this argument first, for a credential of
/// identical shape, in migration `20260905130000`; refresh tokens were left behind. The
/// prefix here is the same domain-separation idiom, with a different label, so the two
/// digests can never coincide over the same bytes.
///
/// No secret is mixed in, deliberately. Keying this to `JWT_SECRET` would mean that
/// rotating that secret — which the split plan schedules for Phase 7 — silently invalidates
/// every stored refresh token and signs out every device in the estate.
pub fn hash_refresh_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"teddy-fyi/refresh-token/v1:");
    hasher.update(token.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect()
}

/// Checks a presented refresh token against a stored hash, in **either** format.
///
/// The dual read is the whole reason this can ship without an outage. Sessions live seven
/// days and every row in the table right now holds an Argon2 digest, which cannot be
/// recomputed as a SHA-256 — there is no migration that converts them, because the only
/// input that could do it is the token itself, which the server has never stored. Dropping
/// the rows the way `20260905130000` dropped the ten-minute device codes would sign out
/// every device on the estate.
///
/// So a stored hash is read by its own format, and the *write* side is what migrates: every
/// `issue_session` and every rotation in `refresh_handler` stores the new form, so a session
/// upgrades on its first refresh — minutes of activity, not days — and the Argon2 branch
/// drains on its own. It can be deleted once no row starting with `$argon2` remains
/// (`SELECT count(*) FROM sessions WHERE refresh_token_hash LIKE '$argon2%'`), which the
/// seven-day `expires_at` bounds even for a device that never comes back.
///
/// Neither branch can panic. The legacy one returns `false` on an unparseable stored hash,
/// where it used to `.expect`: a malformed row is one caller re-authenticating, not one
/// panic per refresh attempt.
pub fn verify_refresh_token(hash: &str, token: &str) -> bool {
    if hash.starts_with(ARGON2_PHC_PREFIX) {
        return match argon2::PasswordHash::new(hash) {
            Ok(parsed) => Argon2::default()
                .verify_password(token.as_bytes(), &parsed)
                .is_ok(),
            Err(err) => {
                // Not a panic and not a 500: a row we cannot read is a credential we cannot
                // confirm, which is exactly what "does not match" means. Logged at error
                // because an unreadable hash is our own data bug, not caller behaviour.
                tracing::error!(
                    parse_error = %err,
                    "Stored refresh token hash is not valid Argon2 PHC; treating it as no match"
                );
                false
            }
        };
    }

    constant_time_eq(hash.as_bytes(), hash_refresh_token(token).as_bytes())
}

/// Compares two byte strings in time that does not depend on where they first differ.
///
/// Hand-rolled rather than pulling in `subtle`, because it is six lines and this is the
/// only caller. Argon2's `verify_password` did this internally, and dropping to a plain
/// `==` on the way out would be a quiet downgrade: the value compared is derived from a
/// live credential, and the length check is safe to leak because both sides are
/// fixed-width hex digests of the same hash function.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}
