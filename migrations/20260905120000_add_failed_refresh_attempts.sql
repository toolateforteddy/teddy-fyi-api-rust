-- Counts consecutive failed refresh attempts against a single session.
--
-- `/auth/refresh` is unauthenticated by construction: the refresh token in the body is the
-- only credential, so a caller who guesses wrong has proved nothing except that they are
-- guessing. Guessing must not destroy the session -- doing so turned the endpoint into an
-- unauthenticated remote logout for anyone who knew a `user_id` and a `client_uuid` -- but
-- it must not be silently free either. This counter is the record of it: every successful
-- rotation resets it to zero, so a session carrying a large value is a session somebody is
-- hammering, visible in the logs and in the table itself.
ALTER TABLE sessions ADD COLUMN failed_refresh_attempts INTEGER NOT NULL DEFAULT 0;
