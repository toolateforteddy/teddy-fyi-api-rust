CREATE TABLE sessions (
    user_id TEXT NOT NULL,
    client_uuid TEXT NOT NULL,
    refresh_token_hash TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (user_id, client_uuid)
);

CREATE INDEX idx_sessions_expires_at ON sessions(expires_at);