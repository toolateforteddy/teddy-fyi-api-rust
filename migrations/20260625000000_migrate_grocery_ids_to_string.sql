-- Drop foreign keys first to allow type changes
ALTER TABLE grocery_items DROP CONSTRAINT IF EXISTS "grocery_items_categoryId_fkey";
ALTER TABLE grocery_item_store_info DROP CONSTRAINT IF EXISTS "grocery_item_store_info_groceryItemId_fkey";
ALTER TABLE grocery_item_store_info DROP CONSTRAINT IF EXISTS "grocery_item_store_info_storeId_fkey";

-- Alter column types to TEXT
ALTER TABLE categories ALTER COLUMN id DROP DEFAULT;
ALTER TABLE categories ALTER COLUMN id TYPE TEXT USING id::text;

ALTER TABLE stores ALTER COLUMN id DROP DEFAULT;
ALTER TABLE stores ALTER COLUMN id TYPE TEXT USING id::text;

ALTER TABLE grocery_items ALTER COLUMN id DROP DEFAULT;
ALTER TABLE grocery_items ALTER COLUMN id TYPE TEXT USING id::text;
ALTER TABLE grocery_items ALTER COLUMN "categoryId" TYPE TEXT USING "categoryId"::text;

ALTER TABLE grocery_item_store_info ALTER COLUMN "groceryItemId" TYPE TEXT USING "groceryItemId"::text;
ALTER TABLE grocery_item_store_info ALTER COLUMN "storeId" TYPE TEXT USING "storeId"::text;

-- Recreate foreign key constraints
ALTER TABLE grocery_items 
  ADD CONSTRAINT "grocery_items_categoryId_fkey" 
  FOREIGN KEY ("categoryId") REFERENCES categories(id) ON DELETE SET NULL;

ALTER TABLE grocery_item_store_info 
  ADD CONSTRAINT "grocery_item_store_info_groceryItemId_fkey" 
  FOREIGN KEY ("groceryItemId") REFERENCES grocery_items(id) ON DELETE CASCADE;

ALTER TABLE grocery_item_store_info 
  ADD CONSTRAINT "grocery_item_store_info_storeId_fkey" 
  FOREIGN KEY ("storeId") REFERENCES stores(id) ON DELETE CASCADE;

-- Drop sequences as they are no longer needed
DROP SEQUENCE IF EXISTS categories_id_seq;
DROP SEQUENCE IF EXISTS stores_id_seq;
DROP SEQUENCE IF EXISTS grocery_items_id_seq;
