-- Add is_deleted and sync_state to grocery_lists
ALTER TABLE grocery_lists ADD COLUMN IF NOT EXISTS is_deleted BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE grocery_lists ADD COLUMN IF NOT EXISTS sync_state TEXT NOT NULL DEFAULT 'SYNCED';

-- Add is_deleted and sync_state to grocery_list_members
ALTER TABLE grocery_list_members ADD COLUMN IF NOT EXISTS is_deleted BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE grocery_list_members ADD COLUMN IF NOT EXISTS sync_state TEXT NOT NULL DEFAULT 'SYNCED';

-- Add is_deleted and sync_state to stores
ALTER TABLE stores ADD COLUMN IF NOT EXISTS is_deleted BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE stores ADD COLUMN IF NOT EXISTS sync_state TEXT NOT NULL DEFAULT 'SYNCED';

-- Add is_deleted and sync_state to categories
ALTER TABLE categories ADD COLUMN IF NOT EXISTS is_deleted BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE categories ADD COLUMN IF NOT EXISTS sync_state TEXT NOT NULL DEFAULT 'SYNCED';

-- Add is_deleted and sync_state to grocery_item_store_info
ALTER TABLE grocery_item_store_info ADD COLUMN IF NOT EXISTS is_deleted BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE grocery_item_store_info ADD COLUMN IF NOT EXISTS sync_state TEXT NOT NULL DEFAULT 'SYNCED';
