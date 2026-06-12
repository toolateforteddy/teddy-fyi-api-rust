-- Add icon column to todo_items and categories tables
ALTER TABLE todo_items ADD COLUMN icon TEXT;
ALTER TABLE categories ADD COLUMN icon TEXT;
