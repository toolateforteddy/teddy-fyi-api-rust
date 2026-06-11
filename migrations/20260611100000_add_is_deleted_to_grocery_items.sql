-- Add is_deleted to grocery_items table
ALTER TABLE grocery_items
ADD COLUMN is_deleted BOOLEAN NOT NULL DEFAULT FALSE;
