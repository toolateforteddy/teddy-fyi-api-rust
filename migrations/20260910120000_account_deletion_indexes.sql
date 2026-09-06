-- Indexes for the columns `DELETE /api/user/data` filters on.
--
-- The erasure path deletes a user's rows table by table, and three of those filters had
-- no index behind them, so each was a sequential scan of the whole table:
--
--   * grocery_items."userId"           -- PK is `id`; only "listId" was indexed
--   * grocery_item_store_info."userId" -- PK is ("groceryItemId", "storeId")
--   * device_authorizations.user_id    -- PK is device_code_hash; user_code/client_uuid/
--                                      -- expires_at are indexed, user_id was not
--
-- Deletion runs once per account, so this is not a hot path. It is, however, the path
-- that must not time out: `jobs::reap_stale_users` drives it in bulk from a CronJob, and
-- a scan per table per user is how that job starts taking longer than its schedule.
--
-- `grocery_items."userId"` is the one with a second caller. It is measurably used by the
-- deletion filter (`"userId" = $1 OR "listId" = ANY($2)` plans as a BitmapOr over this
-- index and idx_grocery_items_listId). It is deliberately NOT a
-- ("userId", updated_at) composite: the sync download's private-items branch sits inside
-- an OR that spans a LEFT JOIN, which no index on this table can serve, so the
-- updated_at half would pay write cost for a read that cannot use it. If that query is
-- ever restructured into index-friendly branches, the composite belongs with that change.
--
-- No CONCURRENTLY: sqlx runs each migration inside a transaction and would reject it.

CREATE INDEX IF NOT EXISTS "idx_grocery_items_userId"
    ON grocery_items("userId");

CREATE INDEX IF NOT EXISTS "idx_grocery_item_store_info_userId"
    ON grocery_item_store_info("userId");

CREATE INDEX IF NOT EXISTS idx_device_authorizations_user_id
    ON device_authorizations(user_id);
