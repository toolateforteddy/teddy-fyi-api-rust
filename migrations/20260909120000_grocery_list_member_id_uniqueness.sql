-- One live membership row per (list, user), enforced by the database rather than by the
-- shape of a string.
--
-- Until now `grocery_list_members.id` was `format!("{}-member-{}", list_id, user_id)`, and
-- that derivation was doing two jobs at once. The visible one was a privacy defect: the id
-- syncs verbatim to every other member of the list, so joining a shared grocery list
-- disclosed the joiner's raw Google subject to everyone else on it. The invisible one is
-- why the defect could not simply be deleted: both writers rely on the id being derivable
-- to stay idempotent --
--
--   * `/api/lists/join` upserts `ON CONFLICT (id) DO UPDATE SET is_deleted = FALSE`, which
--     is how re-joining a list you left revives your row instead of adding a second one;
--   * the sync path seeds the list creator's ADMIN row with `ON CONFLICT (id) DO NOTHING`.
--
-- Take the derivation away and both lose their conflict target. So the uniqueness moves
-- from the id to the pair it was encoding, and the id becomes a `gen_random_uuid()` that
-- says nothing about anybody.
--
-- **Existing rows keep their existing ids.** Rewriting them would mean a client-visible
-- primary key changing underneath devices that hold it, with no tombstone to explain the
-- old one -- a ghost row on every phone in the household. The upserts now match on
-- ("listId", "userId"), so an existing row is still found and revived under its old id;
-- what changes is that no *new* row is ever minted with a subject inside it.
--
-- Note what this does not fix, because the survey's item 18 does not cover it and it is a
-- wire-contract change rather than a schema one: `GroceryListMemberData.user_id` still
-- carries the raw subject to co-members on every sync. The id was one of two channels.

-- Duplicates first, or the index below cannot be created.
--
-- Post-`/api/lists/join`-authorisation there is no way to create a second row for a pair:
-- the two writers above are the only ones, and both derive the same id. Rows predating
-- that fix are a different matter -- sync used to accept a client-invented membership row
-- with any id, which is the hole PR #56 closed -- so a pair may well have more than one
-- row in a database that has been running since June.
--
-- Which row survives: a live one over a deleted one (deleting somebody's working
-- membership to satisfy an index would be an outage for them), then the earliest
-- `joinedAt`, then the lexically smallest id so the choice is deterministic and this
-- migration is reproducible. The rows it removes are duplicates that no correct write path
-- could have produced; a client still holding one keeps a stale local row until its next
-- full sync, which is the cheaper end of the trade against refusing to start.
DELETE FROM grocery_list_members a
USING grocery_list_members b
WHERE a."listId" = b."listId"
  AND a."userId" = b."userId"
  AND a.id <> b.id
  AND (a.is_deleted, a."joinedAt", a.id) > (b.is_deleted, b."joinedAt", b.id);

CREATE UNIQUE INDEX IF NOT EXISTS "uq_grocery_list_members_listId_userId"
    ON grocery_list_members ("listId", "userId");

-- Superseded: a unique btree on the same two columns in the same order serves every lookup
-- the plain index served (it was added for the membership checks in 20260908120000), and
-- keeping both would cost a second index write on every membership change to buy nothing.
DROP INDEX IF EXISTS "idx_grocery_list_members_listId_userId";
