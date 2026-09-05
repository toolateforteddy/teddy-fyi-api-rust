#!/usr/bin/env bash
# SessionStart hook: say whether this machine can run the test suite.
#
# Around 190 of the tests are #[sqlx::test] and need a real Postgres -- without one they
# fail rather than skip. The Redis-backed tests do the opposite and print SKIP, so
# a suite run without Redis is green on cases that never ran. Both are installed on
# the standard container image and simply not started, which is the single most
# common reason a session decides, wrongly, that it cannot test its change.
#
# Reports only. Starting the services is validate.sh's job. Always exits 0.

set -uo pipefail

DATABASE_URL=${DATABASE_URL:-postgres://postgres:password@localhost:5432/test_db}
REDIS_URL=${REDIS_URL:-redis://localhost:6379}

db=down
psql "$DATABASE_URL" -Atc 'select 1' >/dev/null 2>&1 && db=up

cache=down
command -v redis-cli >/dev/null 2>&1 && redis-cli -u "$REDIS_URL" ping >/dev/null 2>&1 && cache=up

[ "$db" = up ] && [ "$cache" = up ] && exit 0

installed=""
command -v pg_isready >/dev/null 2>&1 && installed="Postgres"
command -v redis-server >/dev/null 2>&1 && installed="${installed:+$installed and }Redis"

if [ "$db" = down ] && [ "$cache" = down ]; then
  what="Neither Postgres nor Redis is answering"
elif [ "$db" = down ]; then
  what="Postgres is not answering (Redis is)"
else
  what="Redis is not answering (Postgres is)"
fi

if [ -n "$installed" ]; then
  msg="$what. $installed is installed here, just not started -- ./validate.sh starts what is missing and then runs the full gate. Do not conclude that the tests cannot be run in this container."
else
  msg="$what, and no server is installed here. The ~190 sqlx::test cases will fail rather than skip; ./validate.sh --no-db runs clippy and the constraint checks instead."
fi

printf '{"systemMessage":"%s","hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"%s"}}\n' "$msg" "$msg"
