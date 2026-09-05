-- `drawings.last_modified` and `configs.last_modified` become server-stamped.
--
-- Both columns were written verbatim from the request body, and both do two jobs the
-- client is not entitled to decide:
--
--   * they broke version conflicts (larger stamp wins), so a device with a wrong clock --
--     or one that simply sends a stamp far in the future -- won every conflict on the
--     account, permanently, overwriting another tablet's drawings and settings;
--   * they *are* the download cursor (`WHERE last_modified > $last_synced_ms`), so a
--     future stamp replays that row to every device on every sync forever and a past one
--     hides it from the sibling tablet entirely.
--
-- From here the server writes its own request timestamp there. The client's claim is not
-- thrown away -- it is the only record of when the edit actually happened on a device
-- that was offline at the time, which is worth keeping for display and for support --
-- so it moves to its own column, where nothing on the server compares it.
--
-- Nullable with no default and no backfill: rows written before this migration have no
-- separate client claim to record, and NULL says exactly that rather than inventing one.

ALTER TABLE drawings ADD COLUMN IF NOT EXISTS client_last_modified BIGINT;
ALTER TABLE configs ADD COLUMN IF NOT EXISTS client_last_modified BIGINT;

COMMENT ON COLUMN drawings.client_last_modified IS
    'When the writing device claimed the edit happened. Descriptive only: never compared, ordered by, or used as a sync cursor. See migrations/20260907141500.';
COMMENT ON COLUMN configs.client_last_modified IS
    'When the writing device claimed the edit happened. Descriptive only: never compared, ordered by, or used as a sync cursor. See migrations/20260907141500.';
