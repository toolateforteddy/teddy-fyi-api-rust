-- Indexes for the predicates the /sync endpoint actually runs.
--
-- Every one of these columns is filtered or joined on once per sync request (several of
-- them once per item in the payload), and until now none of them was indexed: the only
-- grocery indexes were the primary keys plus the single-column "listId" indexes added
-- with collaborative lists. On an empty dev database a sequential scan is free, which is
-- why this went unnoticed; on a real account it is a full table scan per row uploaded.
--
-- CONCURRENTLY is deliberately not used: sqlx runs each migration inside a transaction
-- and CREATE INDEX CONCURRENTLY cannot run there. These tables are small enough that the
-- brief ACCESS EXCLUSIVE lock during the build is acceptable.

-- grocery_items.rs auto-populates store mappings for a newly inserted item by looking up
-- every existing item with the same name, case-insensitively:
--   WHERE LOWER(gi.name) = LOWER($1)
-- A plain index on name cannot serve that, so this is a functional index on the exact
-- expression. It runs once per grocery item in the upload, so it is the single most
-- valuable index in this migration.
CREATE INDEX IF NOT EXISTS "idx_grocery_items_lower_name" ON grocery_items (LOWER(name));

-- grocery_list_members is joined or filtered on "userId" by essentially every grocery
-- query (all six process_* upload paths, the download path, and affected_users), and only
-- the implicit primary key on id existed. The ("listId", "userId") composite serves the
-- membership-of-this-list lookups; the standalone "userId" index serves the
-- which-lists-does-this-user-belong-to direction, which the composite cannot because
-- "userId" is not its leading column.
CREATE INDEX IF NOT EXISTS "idx_grocery_list_members_userId"
    ON grocery_list_members ("userId");
CREATE INDEX IF NOT EXISTS "idx_grocery_list_members_listId_userId"
    ON grocery_list_members ("listId", "userId");

-- The incremental-download queries in grocery/remote_mutations.rs and
-- todo/remote_mutations.rs all have the same shape: narrow by owner or list, then
-- `updated_at > $last_synced_at`. Putting updated_at second lets the range be satisfied
-- from the index instead of rechecking every row of the scoped partition.
CREATE INDEX IF NOT EXISTS "idx_grocery_items_listId_updated_at"
    ON grocery_items ("listId", updated_at);
CREATE INDEX IF NOT EXISTS "idx_grocery_lists_updated_at"
    ON grocery_lists (updated_at);
CREATE INDEX IF NOT EXISTS "idx_grocery_lists_ownerId"
    ON grocery_lists ("ownerId");
CREATE INDEX IF NOT EXISTS "idx_grocery_list_members_listId_updated_at"
    ON grocery_list_members ("listId", updated_at);
CREATE INDEX IF NOT EXISTS "idx_stores_userId_updated_at"
    ON stores ("userId", updated_at);
CREATE INDEX IF NOT EXISTS "idx_stores_listId_updated_at"
    ON stores ("listId", updated_at);
CREATE INDEX IF NOT EXISTS "idx_categories_userId_updated_at"
    ON categories ("userId", updated_at);
CREATE INDEX IF NOT EXISTS "idx_categories_listId_updated_at"
    ON categories ("listId", updated_at);
CREATE INDEX IF NOT EXISTS "idx_grocery_item_store_info_storeId_updated_at"
    ON grocery_item_store_info ("storeId", updated_at);
CREATE INDEX IF NOT EXISTS "idx_todo_items_userId_updated_at"
    ON todo_items ("userId", updated_at);
CREATE INDEX IF NOT EXISTS "idx_todo_lists_userId_updated_at"
    ON todo_lists ("userId", updated_at);

-- config.rs and drawing.rs both download with
--   WHERE user_id = $1 AND last_modified > $2 AND ($n::uuid IS NULL OR device_uuid = $n)
-- so the three columns are always used together. Only single-column user_id and
-- device_uuid indexes existed, which forced a heap recheck of the user's whole history on
-- every sync. device_uuid sits in the middle because it is an equality predicate when
-- present and the index still degrades gracefully to its user_id prefix when the device
-- filter is absent.
CREATE INDEX IF NOT EXISTS idx_configs_user_id_device_uuid_last_modified
    ON configs (user_id, device_uuid, last_modified);
CREATE INDEX IF NOT EXISTS idx_drawings_user_id_device_uuid_last_modified
    ON drawings (user_id, device_uuid, last_modified);
