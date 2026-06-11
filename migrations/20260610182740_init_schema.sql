-- Migration script generated from Android Room schema

-- Table: todo_lists
CREATE TABLE "todo_lists" (
    "id" TEXT PRIMARY KEY,
    "name" TEXT NOT NULL,
    "colorHex" TEXT NOT NULL,
    "userId" TEXT,
    "createdAt" BIGINT NOT NULL,
    "sync_state" TEXT NOT NULL,
    "version" INTEGER NOT NULL DEFAULT 1,
    "is_deleted" BOOLEAN NOT NULL DEFAULT FALSE,
    "updated_at" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_by_client" TEXT
);

-- Table: todo_items
CREATE TABLE "todo_items" (
    "id" TEXT PRIMARY KEY,
    "title" TEXT NOT NULL,
    "isCompleted" BOOLEAN NOT NULL,
    "createdAt" BIGINT NOT NULL,
    "position" INTEGER NOT NULL,
    "scheduledDate" TEXT,
    "recurrenceRule" TEXT,
    "scheduledAt" BIGINT NOT NULL,
    "userId" TEXT,
    "parentId" TEXT,
    "isDaily" BOOLEAN NOT NULL,
    "dueDate" BIGINT,
    "description" TEXT,
    "listId" TEXT REFERENCES "todo_lists"("id") ON DELETE SET NULL,
    "priority" INTEGER NOT NULL,
    "sync_state" TEXT NOT NULL,
    "version" INTEGER NOT NULL DEFAULT 1,
    "is_deleted" BOOLEAN NOT NULL DEFAULT FALSE,
    "updated_at" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_by_client" TEXT
);

CREATE INDEX "idx_todo_items_listId" ON "todo_items"("listId");

-- Table: grocery_lists
CREATE TABLE "grocery_lists" (
    "id" TEXT PRIMARY KEY,
    "name" TEXT NOT NULL,
    "ownerId" TEXT,
    "createdAt" BIGINT NOT NULL,
    "version" INTEGER NOT NULL DEFAULT 1,
    "updated_at" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_by_client" TEXT
);

-- Table: grocery_list_members
CREATE TABLE "grocery_list_members" (
    "id" TEXT PRIMARY KEY,
    "listId" TEXT NOT NULL REFERENCES "grocery_lists"("id") ON DELETE CASCADE,
    "userId" TEXT NOT NULL,
    "role" TEXT NOT NULL,
    "joinedAt" BIGINT NOT NULL,
    "version" INTEGER NOT NULL DEFAULT 1,
    "updated_at" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_by_client" TEXT
);

-- Table: stores
CREATE TABLE "stores" (
    "id" SERIAL PRIMARY KEY,
    "name" TEXT NOT NULL,
    "position" INTEGER NOT NULL,
    "isDefaultSupported" BOOLEAN NOT NULL,
    "userId" TEXT,
    "version" INTEGER NOT NULL DEFAULT 1,
    "updated_at" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_by_client" TEXT
);

-- Table: categories
CREATE TABLE "categories" (
    "id" SERIAL PRIMARY KEY,
    "name" TEXT NOT NULL,
    "position" INTEGER NOT NULL,
    "userId" TEXT,
    "version" INTEGER NOT NULL DEFAULT 1,
    "updated_at" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_by_client" TEXT
);

-- Table: grocery_items
CREATE TABLE "grocery_items" (
    "id" SERIAL PRIMARY KEY,
    "name" TEXT NOT NULL,
    "quantity" TEXT NOT NULL,
    "isBought" BOOLEAN NOT NULL,
    "createdAt" BIGINT NOT NULL,
    "position" INTEGER NOT NULL,
    "categoryId" INTEGER,
    "timesBought" INTEGER NOT NULL,
    "userId" TEXT,
    "isActive" BOOLEAN NOT NULL,
    "listId" TEXT REFERENCES "grocery_lists"("id") ON DELETE SET NULL,
    "unit" TEXT,
    "notes" TEXT,
    "version" INTEGER NOT NULL DEFAULT 1,
    "updated_at" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_by_client" TEXT
);

CREATE INDEX "idx_grocery_items_listId" ON "grocery_items"("listId");

-- Table: grocery_item_store_info
CREATE TABLE "grocery_item_store_info" (
    "groceryItemId" INTEGER NOT NULL REFERENCES "grocery_items"("id") ON DELETE CASCADE,
    "storeId" INTEGER NOT NULL REFERENCES "stores"("id") ON DELETE CASCADE,
    "price" DOUBLE PRECISION,
    "isAvailable" BOOLEAN NOT NULL,
    "userId" TEXT,
    "version" INTEGER NOT NULL DEFAULT 1,
    "updated_at" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_by_client" TEXT,
    PRIMARY KEY ("groceryItemId", "storeId")
);

CREATE INDEX "idx_grocery_item_store_info_groceryItemId" ON "grocery_item_store_info"("groceryItemId");
CREATE INDEX "idx_grocery_item_store_info_storeId" ON "grocery_item_store_info"("storeId");
