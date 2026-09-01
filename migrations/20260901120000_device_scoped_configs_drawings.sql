-- Device-scope configs and drawings so one account can own several tablets.
--
-- `client_uuid` is NOT device identity: it records which client instance last wrote a
-- row and exists purely for echo suppression. `device_uuid` is the new, separate
-- identity for a physical tablet (the ScribbleBox + ScribbleKeep pair share one).

-- No FK to "users": users.id is TEXT (the auth subject) while configs.user_id is a UUID
-- derived from it via parse_or_hash_uuid, so the types genuinely do not line up.
CREATE TABLE IF NOT EXISTS devices (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_devices_user_id ON devices(user_id);

ALTER TABLE configs ADD COLUMN IF NOT EXISTS device_uuid UUID;
ALTER TABLE drawings ADD COLUMN IF NOT EXISTS device_uuid UUID;

-- Backfill one device per existing user. The tablet id is promoted from the client's own
-- `box_client_uuid` config row where it is a well-formed UUID, so existing tablets claim
-- their own data without re-pairing; anything else gets a fresh id.
DO $$
DECLARE
    u RECORD;
    dev_id UUID;
    dev_name TEXT;
    uuid_re CONSTANT TEXT :=
        '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$';
BEGIN
    FOR u IN
        SELECT user_id FROM configs
        UNION
        SELECT user_id FROM drawings
    LOOP
        SELECT b.value::uuid INTO dev_id
        FROM configs b
        WHERE b.user_id = u.user_id
          AND b.key = 'box_client_uuid'
          AND b.value ~* uuid_re
        ORDER BY b.last_modified DESC
        LIMIT 1;

        -- Guard the cast result: fall back rather than failing the migration, and never
        -- let two accounts land on the same device id.
        IF dev_id IS NULL OR EXISTS (SELECT 1 FROM devices d WHERE d.id = dev_id) THEN
            dev_id := gen_random_uuid();
        END IF;

        SELECT n.value INTO dev_name
        FROM configs n
        WHERE n.user_id = u.user_id
          AND n.key = 'device_name'
          AND n.value <> ''
        ORDER BY n.last_modified DESC
        LIMIT 1;

        IF dev_name IS NULL THEN
            dev_name := 'Tablet';
        END IF;

        INSERT INTO devices (id, user_id, name) VALUES (dev_id, u.user_id, dev_name);

        UPDATE configs SET device_uuid = dev_id
         WHERE user_id = u.user_id AND device_uuid IS NULL;
        UPDATE drawings SET device_uuid = dev_id
         WHERE user_id = u.user_id AND device_uuid IS NULL;
    END LOOP;
END $$;

ALTER TABLE configs ALTER COLUMN device_uuid SET NOT NULL;
ALTER TABLE drawings ALTER COLUMN device_uuid SET NOT NULL;

CREATE INDEX IF NOT EXISTS idx_configs_device_uuid ON configs(device_uuid);
CREATE INDEX IF NOT EXISTS idx_drawings_device_uuid ON drawings(device_uuid);

-- Two tablets on one account must be able to hold the same config key independently.
ALTER TABLE configs DROP CONSTRAINT IF EXISTS unique_user_config_key;
ALTER TABLE configs
    ADD CONSTRAINT unique_user_device_config_key UNIQUE (user_id, device_uuid, key);
