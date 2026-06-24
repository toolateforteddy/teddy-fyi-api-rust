-- Add listId column to stores table
ALTER TABLE stores
ADD COLUMN IF NOT EXISTS "listId" TEXT REFERENCES "grocery_lists"("id") ON DELETE SET NULL;

-- Add listId column to categories table
ALTER TABLE categories
ADD COLUMN IF NOT EXISTS "listId" TEXT REFERENCES "grocery_lists"("id") ON DELETE SET NULL;

-- Create indices to optimize query scopes
CREATE INDEX IF NOT EXISTS "idx_stores_listId" ON stores("listId");
CREATE INDEX IF NOT EXISTS "idx_categories_listId" ON categories("listId");
CREATE INDEX IF NOT EXISTS "idx_grocery_items_listId" ON grocery_items("listId");

-- Create list_invites table
CREATE TABLE IF NOT EXISTS list_invites (
    code TEXT PRIMARY KEY,
    "listId" TEXT NOT NULL REFERENCES "grocery_lists"("id") ON DELETE CASCADE,
    "createdBy" TEXT NOT NULL,
    "expiresAt" TIMESTAMPTZ NOT NULL
);

-- Create index on listId for invites table
CREATE INDEX IF NOT EXISTS "idx_list_invites_listId" ON list_invites("listId");
