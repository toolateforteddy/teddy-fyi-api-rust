-- Add the surrogate user id column.
--
-- `context/2026-09-05_identity_model.md` replaces the raw Google subject that keys
-- `users` today with an opaque UUID derived from nothing. That cutover happens in
-- Phase 5 of the split, during the write freeze, when the copy program mints the
-- ids. This migration only puts the column in place ahead of it.
--
-- Nothing reads or writes `surrogate_id` yet: no handler, no token claim, no client
-- field. It is server-only and invisible from the wire.
--
-- `gen_random_uuid()` is volatile, so Postgres evaluates it once per row while it
-- rewrites the table for the ADD COLUMN -- existing users are backfilled by the
-- ALTER itself and every row gets a distinct value. The rewrite holds ACCESS
-- EXCLUSIVE on `users` for its duration; the table is small enough that this is a
-- blip, and nothing else here depends on the column being present.
--
-- The column stays nullable and carries no NOT NULL: nothing populates it explicitly,
-- and tightening it belongs with the cutover that starts writing it.
ALTER TABLE "users"
    ADD COLUMN IF NOT EXISTS "surrogate_id" UUID UNIQUE DEFAULT gen_random_uuid();
