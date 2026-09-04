-- Device pairing: a Fire tablet (no Google Play Services, so no on-device Google
-- identity) asks for a short code and polls; the parent redeems that code from a
-- browser that does have a Google account, and the tablet collects the same
-- access/refresh pair `/auth/login` mints.
--
-- Postgres and not Valkey on purpose: this is auth state, it must not evaporate with
-- the cache, and unlike the sync-status cache there is no cheap query that could
-- recompute it.

CREATE TABLE IF NOT EXISTS device_authorizations (
    -- Argon2 of the device code, hashed by auth::tokens::hash_refresh_token exactly as
    -- refresh tokens are: a database dump must not yield a usable code. The hash is
    -- salted, so it is a primary key for uniqueness only and never a lookup key --
    -- `poll` narrows by client_uuid and verifies the candidates.
    device_code_hash TEXT PRIMARY KEY,
    -- What the parent types. Stored in the clear: it is eight characters, single-use,
    -- and lives ten minutes, and it has to be looked up by exactly the value typed.
    user_code TEXT NOT NULL UNIQUE,
    -- Must match the client_uuid that called /start, so a leaked device code is not
    -- portable to another install.
    client_uuid TEXT NOT NULL,
    -- Which app asked, for diagnostics. Not part of any check.
    app TEXT,
    -- Null until a parent claims the code.
    user_id TEXT,
    -- Failed claims against this row.
    attempts INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    -- Rate-limits polling without a second store; see the `interval` in /start.
    last_polled_at TIMESTAMPTZ,
    -- Claimed = a parent redeemed it. Consumed = the tablet collected the tokens.
    claimed_at TIMESTAMPTZ,
    consumed_at TIMESTAMPTZ
);

-- The claim path's only lookup.
CREATE INDEX IF NOT EXISTS idx_device_authorizations_user_code
    ON device_authorizations(user_code);
-- The poll path narrows by client_uuid before verifying hashes; the reaper sweeps
-- by expires_at.
CREATE INDEX IF NOT EXISTS idx_device_authorizations_client_uuid
    ON device_authorizations(client_uuid);
CREATE INDEX IF NOT EXISTS idx_device_authorizations_expires_at
    ON device_authorizations(expires_at);

-- Failed claims, counted per Google account rather than per row: a parent mistyping a
-- code produces no row to count against, which is exactly the case the limit exists
-- for. Rows older than the window are dead weight and the reaper drops them.
CREATE TABLE IF NOT EXISTS device_claim_failures (
    id BIGSERIAL PRIMARY KEY,
    user_id TEXT NOT NULL,
    failed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_device_claim_failures_user_id_failed_at
    ON device_claim_failures(user_id, failed_at);
