-- Brute-force and quota defences for grocery-list invites.
--
-- An invite code is eight uppercase alphanumerics -- a ~2.8e12 space, but nothing in
-- `/api/lists/join` made a guesser pay for a wrong answer: unlimited attempts, no counter,
-- and a hit grants MEMBER on another family's list (read *and* write). The two columns and
-- the one table below are the same shape the device-pairing flow already uses
-- (`device_authorizations.attempts` and `device_claim_failures`), on purpose: two
-- brute-force defences that look alike are two that get reasoned about the same way.

-- Failed redemptions counted against this specific code. A guess that matches nothing
-- cannot be attributed to a row -- that case is what `list_join_failures` below is for --
-- but a code that exists and is refused is a code somebody is poking at, and past
-- `MAX_INVITE_ATTEMPTS` the handler deletes it rather than leaving it as a standing
-- oracle.
ALTER TABLE list_invites ADD COLUMN IF NOT EXISTS attempts INT NOT NULL DEFAULT 0;

-- The outstanding-invite cap counts a user's unexpired rows on every invite request, so
-- that count needs an index of its own; the existing one is on "listId".
CREATE INDEX IF NOT EXISTS "idx_list_invites_createdBy_expiresAt"
    ON list_invites("createdBy", "expiresAt");

-- Failed joins, counted per account rather than per row: a guesser's wrong code matches no
-- invite at all, which is exactly the case the limit exists for. Mirrors
-- `device_claim_failures` down to the column names. Rows older than the window are dead
-- weight and the reaper drops them.
CREATE TABLE IF NOT EXISTS list_join_failures (
    id BIGSERIAL PRIMARY KEY,
    user_id TEXT NOT NULL,
    failed_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_list_join_failures_user_id_failed_at
    ON list_join_failures(user_id, failed_at);
