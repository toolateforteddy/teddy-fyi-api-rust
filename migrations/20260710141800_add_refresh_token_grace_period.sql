-- Add columns for refresh token grace period / breach mitigation mitigation
ALTER TABLE sessions ADD COLUMN old_refresh_token_hash TEXT;
ALTER TABLE sessions ADD COLUMN rotated_at TIMESTAMPTZ;
