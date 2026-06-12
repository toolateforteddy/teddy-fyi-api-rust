-- Add sync_state to grocery_items
ALTER TABLE grocery_items ADD COLUMN IF NOT EXISTS sync_state TEXT NOT NULL DEFAULT 'SYNCED';

-- Ensure grocery_item_store_info has the columns too (in case the previous migration was run before they were added)
ALTER TABLE grocery_item_store_info ADD COLUMN IF NOT EXISTS is_deleted BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE grocery_item_store_info ADD COLUMN IF NOT EXISTS sync_state TEXT NOT NULL DEFAULT 'SYNCED';
