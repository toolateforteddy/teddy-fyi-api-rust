# Legacy Database Migration Guide

This document outlines the steps required to migrate the legacy database schema (using `TEXT` for UUIDs and sync states) to native PostgreSQL `UUID` and `sync_state` enum types.

---

## 1. Database Schema Migration Script

To update the existing tables in production, we will need a migration script that safely converts the columns while preserving data integrity and updating dependencies.

### Step 1.1: Temporary Removal of Foreign Keys
PostgreSQL will not allow changing the column type of a primary key if it is referenced by a foreign key constraint. We must temporarily drop the constraints:
```sql
ALTER TABLE todo_items DROP CONSTRAINT IF EXISTS todo_items_listId_fkey;
ALTER TABLE grocery_list_members DROP CONSTRAINT IF EXISTS grocery_list_members_listId_fkey;
ALTER TABLE grocery_items DROP CONSTRAINT IF EXISTS grocery_items_listId_fkey;
ALTER TABLE grocery_item_store_info DROP CONSTRAINT IF EXISTS grocery_item_store_info_groceryItemId_fkey;
```

### Step 1.2: Column Type Conversions
Convert the ID columns from `TEXT` to native `UUID`. PostgreSQL can perform this casting using the `USING` clause:
```sql
-- Convert todo_lists
ALTER TABLE todo_lists 
  ALTER COLUMN id TYPE UUID USING id::uuid,
  ALTER COLUMN "userId" TYPE UUID USING "userId"::uuid;

-- Convert todo_items
ALTER TABLE todo_items 
  ALTER COLUMN id TYPE UUID USING id::uuid,
  ALTER COLUMN "userId" TYPE UUID USING "userId"::uuid,
  ALTER COLUMN "parentId" TYPE UUID USING "parentId"::uuid,
  ALTER COLUMN "listId" TYPE UUID USING "listId"::uuid;

-- Convert grocery_lists
ALTER TABLE grocery_lists 
  ALTER COLUMN id TYPE UUID USING id::uuid,
  ALTER COLUMN "ownerId" TYPE UUID USING "ownerId"::uuid;

-- Convert grocery_list_members
ALTER TABLE grocery_list_members 
  ALTER COLUMN id TYPE UUID USING id::uuid,
  ALTER COLUMN "listId" TYPE UUID USING "listId"::uuid,
  ALTER COLUMN "userId" TYPE UUID USING "userId"::uuid;

-- Convert grocery_items (id is SERIAL, listId is TEXT UUID)
ALTER TABLE grocery_items 
  ALTER COLUMN "userId" TYPE UUID USING "userId"::uuid,
  ALTER COLUMN "listId" TYPE UUID USING "listId"::uuid;

-- Convert grocery_item_store_info
ALTER TABLE grocery_item_store_info 
  ALTER COLUMN "userId" TYPE UUID USING "userId"::uuid;

-- Convert stores and categories
ALTER TABLE stores ALTER COLUMN "userId" TYPE UUID USING "userId"::uuid;
ALTER TABLE categories ALTER COLUMN "userId" TYPE UUID USING "userId"::uuid;

-- Convert users
ALTER TABLE users ALTER COLUMN id TYPE UUID USING id::uuid;

-- Convert sessions
ALTER TABLE sessions 
  ALTER COLUMN user_id TYPE UUID USING user_id::uuid,
  ALTER COLUMN client_uuid TYPE UUID USING client_uuid::uuid;
```

### Step 1.3: Convert sync_state columns to ENUM
Convert the text columns for sync state to use the custom Postgres `sync_state` enum:
```sql
ALTER TABLE todo_lists ALTER COLUMN sync_state TYPE sync_state USING sync_state::sync_state;
ALTER TABLE todo_items ALTER COLUMN sync_state TYPE sync_state USING sync_state::sync_state;
ALTER TABLE grocery_lists ALTER COLUMN sync_state TYPE sync_state USING sync_state::sync_state;
ALTER TABLE grocery_list_members ALTER COLUMN sync_state TYPE sync_state USING sync_state::sync_state;
ALTER TABLE stores ALTER COLUMN sync_state TYPE sync_state USING sync_state::sync_state;
ALTER TABLE categories ALTER COLUMN sync_state TYPE sync_state USING sync_state::sync_state;
ALTER TABLE grocery_items ALTER COLUMN sync_state TYPE sync_state USING sync_state::sync_state;
ALTER TABLE grocery_item_store_info ALTER COLUMN sync_state TYPE sync_state USING sync_state::sync_state;
```

### Step 1.4: Re-establish Foreign Keys
Re-create constraints with the matching `UUID` types:
```sql
ALTER TABLE todo_items ADD CONSTRAINT todo_items_listId_fkey 
  FOREIGN KEY ("listId") REFERENCES todo_lists(id) ON DELETE SET NULL;

ALTER TABLE grocery_list_members ADD CONSTRAINT grocery_list_members_listId_fkey 
  FOREIGN KEY ("listId") REFERENCES grocery_lists(id) ON DELETE CASCADE;

ALTER TABLE grocery_items ADD CONSTRAINT grocery_items_listId_fkey 
  FOREIGN KEY ("listId") REFERENCES grocery_lists(id) ON DELETE SET NULL;
```

---

## 2. Rust Codebase Updates

Once the database changes are applied, the backend Rust service must be updated.

1. **Struct Type Adjustments**:
   In `src/routes/sync/types.rs`, change the types from `String` and `Option<String>` to `uuid::Uuid` and `Option<uuid::Uuid>` for:
   - `TodoListData::id`, `TodoListData::user_id`
   - `TodoItemData::id`, `TodoItemData::user_id`, `TodoItemData::parent_id`, `TodoItemData::list_id`
   - `GroceryListData::id`, `GroceryListData::owner_id`
   - `GroceryListMemberData::id`, `GroceryListMemberData::list_id`, `GroceryListMemberData::user_id`
   - `StoreData::user_id`
   - `CategoryData::user_id`
   - `GroceryItemData::user_id`, `GroceryItemData::list_id`
   - `GroceryItemStoreInfoData::user_id`
   
2. **Enum Conversion**:
   Replace references of `sync_state: String` in the above structs with `sync_state: SyncState` (importing the shared enum).

3. **Claims and Authentication**:
   In `src/auth/tokens.rs`, update `Claims::sub` (the user ID) and `Claims::client_uuid` to be `uuid::Uuid` instead of `String`.

4. **Query Binds & Prepared Queries**:
   Update all queries in `src/routes/sync/` and `src/auth/` to bind `Uuid` parameters directly rather than strings.

---

## 3. Client-Side Mobile App Updates

If the client (Android Room or iOS CoreData/SQLite) is currently serializing UUIDs as Strings in JSON payloads, the API contract remains compatible since `serde` in Rust can automatically deserialize string UUIDs into `uuid::Uuid`.
However, if client-side databases also use string columns, it is recommended to update local entity schemas to `UUID` types to match the backend.

---

## 4. Execution & Rollout Strategy

1. **Backup**: Create a full backup of the production database.
2. **Lockout**: Put the service in read-only/maintenance mode to prevent writes during schema migration.
3. **Migrate DB**: Run the SQL schema migration script.
4. **Deploy Backend**: Deploy the compiled Rust update configured for native UUID query parameters.
5. **Resume**: Remove maintenance mode.
