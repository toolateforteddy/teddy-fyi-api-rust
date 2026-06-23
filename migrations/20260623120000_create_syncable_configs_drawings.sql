-- Create sync_state enum type if not exists
DO $$ BEGIN
    CREATE TYPE sync_state AS ENUM ('SYNCED', 'PENDING_INSERT', 'PENDING_UPDATE', 'PENDING_DELETE');
EXCEPTION
    WHEN duplicate_object THEN null;
END $$;

-- Create configs table
CREATE TABLE IF NOT EXISTS configs (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    client_uuid UUID NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    last_modified BIGINT NOT NULL,
    sync_state sync_state NOT NULL DEFAULT 'SYNCED',
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    CONSTRAINT unique_user_config_key UNIQUE (user_id, key)
);

-- Indices for configs
CREATE INDEX IF NOT EXISTS idx_configs_user_id ON configs(user_id);
CREATE INDEX IF NOT EXISTS idx_configs_client_uuid ON configs(client_uuid);

-- Create drawings table
CREATE TABLE IF NOT EXISTS drawings (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    client_uuid UUID NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    last_modified BIGINT NOT NULL,
    sync_state sync_state NOT NULL DEFAULT 'SYNCED',
    created_at BIGINT NOT NULL,
    data JSONB NOT NULL
);

-- Indices for drawings
CREATE INDEX IF NOT EXISTS idx_drawings_user_id ON drawings(user_id);
CREATE INDEX IF NOT EXISTS idx_drawings_client_uuid ON drawings(client_uuid);
