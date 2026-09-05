-- Which product a session belongs to, so a refresh can re-mint the product claim.
--
-- Versioned 20260908160000 rather than ...120000: that slot was taken by
-- 20260908120000_sync_hot_path_indexes.sql while this branch was open, and two files
-- sharing a version is a duplicate key on `_sqlx_migrations` when both land -- which is
-- how it was found, on the merge commit CI builds rather than on either branch alone.
--
-- The claim itself lives in the access token (auth::tokens::Claims::product), which is
-- good for fifteen minutes. The session behind it is good for seven days and is rotated by
-- POST /auth/refresh, which is unauthenticated: the request body carries a refresh token
-- and nothing else, so there is no audience to re-derive the product from at that point.
-- Without somewhere to keep it, the claim would survive exactly one access-token lifetime
-- and every device would silently fall back to "unclassified" fifteen minutes after
-- signing in, which is the same as not having done this at all.
--
-- The audience *is* available at the two places a session is created — login, and the
-- device-pairing poll — and this column is written there.
--
-- Nullable, and null on every existing row, because that is the truth about them: they
-- were established before anything recorded a product and there is no way to work out
-- retrospectively which client ID they came from. `Product::from_wire` reads null and any
-- unrecognised value as "unknown", and the scope check permits unknown while logging it.
-- See `src/auth/product.rs` for the three-stage rollout this is the middle of.
--
-- TEXT rather than an ENUM to match the rest of this schema's newer columns, and because
-- the values are already spelled once in Rust (`Product::as_wire`); a Postgres type would
-- be a second place to change and a migration to add a product to.
ALTER TABLE sessions ADD COLUMN IF NOT EXISTS product TEXT;

COMMENT ON COLUMN sessions.product IS
    'Product this session was established for (auth::product::Product wire form: teddy_fyi / scribbleroute). NULL = established before the claim existed, or through a client ID that is not classified per product yet.';

-- The same value, carried across the device-pairing handshake.
--
-- A paired tablet never presents a Google token of its own: the parent redeems the code
-- from a browser that does have an account, and the tablet collects the tokens by polling.
-- So the only proof of product in that flow is the audience of the *parent's* ID token, at
-- claim time, and the poll that mints the session happens later in a different request.
--
-- Deliberately not derived from `device_authorizations.app`, which the same table already
-- describes as "for diagnostics, not part of any check": `app` is a string an
-- unauthenticated caller chooses at /auth/device/start, and letting it pick the product
-- would let it pick its own sync scopes -- the exact defect this claim exists to close.
ALTER TABLE device_authorizations ADD COLUMN IF NOT EXISTS product TEXT;

COMMENT ON COLUMN device_authorizations.product IS
    'Product proved by the claiming parent audience (auth::product::Product wire form). NULL = the parent signed in through a client ID that is not classified per product yet.';
