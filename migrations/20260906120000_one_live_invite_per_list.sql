-- At most one live invite code per grocery list.
--
-- Every code minted for a list was a standing target: a family that pressed "invite" three
-- times left three working credentials to their list lying around, each good for a full
-- day, and only one of them was ever the code they actually sent anybody. The other two
-- bought an attacker guesses for free. Minting now supersedes -- `invite_handler` deletes
-- the list's previous rows before inserting the new one -- and this index is what makes
-- that an invariant of the table rather than a property of one code path.
--
-- Existing rows are collapsed to the newest per list first, or the index could not be
-- built. Ties break on the code so the statement is deterministic.
DELETE FROM list_invites a
      USING list_invites b
      WHERE a."listId" = b."listId"
        AND (a."expiresAt", a.code) < (b."expiresAt", b.code);

CREATE UNIQUE INDEX IF NOT EXISTS "idx_list_invites_listId_unique"
    ON list_invites("listId");

-- The unique index serves every lookup the old one did.
DROP INDEX IF EXISTS "idx_list_invites_listId";
