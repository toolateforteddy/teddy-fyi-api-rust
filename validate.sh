#!/usr/bin/env bash
# Validate a change as thoroughly as the current machine allows.
#
# The strong mode is what CI runs -- clippy in both feature configurations, the
# test suite in both, and the SQLx offline-cache check -- plus the constraint
# checks CI does not have (scripts/check_constraints.sh).
#
# All of that needs a Postgres. Around 190 of this repo's ~350 tests are
# `#[sqlx::test]`, which creates a database per test and applies `migrations/` to it,
# so without a server they do not skip: they fail. Redis is softer -- the tests that need it
# print SKIP and pass -- which is worse, because a run with no Redis is green on a
# suite that did not run. This script says which of the two it had.
#
# A cloud container can usually have both, and that is the point of the
# `--services` handling below: Postgres and Redis are already installed on the
# standard image, just not started. Starting them turns a container from "clippy
# only" into the same gate CI runs. What a container cannot do is reach Neon or
# GCP, and nothing here tries to.
#
# Usage:
#   ./validate.sh              start what is missing, run the strongest gate
#   ./validate.sh --full       require a database; fail rather than fall back
#   ./validate.sh --no-db      skip everything needing Postgres
#   ./validate.sh --no-start   use the services that are already up, start none

set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")" || exit 1

MODE=auto
START_SERVICES=1
while [ $# -gt 0 ]; do
    case "$1" in
        --full) MODE=full ;;
        --no-db|--offline) MODE=nodb ;;
        --no-start) START_SERVICES=0 ;;
        # Print the header comment and stop at the first line that is not one, so
        # the block can grow without the line numbers here going stale.
        -h|--help) awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; exit 0 ;;
        *) echo "validate: unknown option $1 (try --help)" >&2; exit 2 ;;
    esac
    shift
done

# The CI credentials, and what scripts/dev.sh does not use -- a dev branch on Neon
# is per-developer and lives in .env. DATABASE_URL from the environment wins, so a
# developer pointed at their own branch database keeps it.
export DATABASE_URL="${DATABASE_URL:-postgres://postgres:password@localhost:5432/test_db}"
export REDIS_URL="${REDIS_URL:-redis://localhost:6379}"
# Every step below compiles against the committed .sqlx/ descriptors rather than a
# database, exactly as CI and the Dockerfile do. The one step that must not is the
# offline-cache check itself, which unsets it.
export SQLX_OFFLINE=true

postgres_ready() {
    # `psql -c 'select 1'` rather than pg_isready: a socket that accepts
    # connections but rejects these credentials is not a database this can use,
    # and finding that out at test time costs a full compile first.
    psql "$DATABASE_URL" -Atc 'select 1' >/dev/null 2>&1
}

redis_ready() {
    command -v redis-cli >/dev/null 2>&1 && redis-cli -u "$REDIS_URL" ping >/dev/null 2>&1
}

# Bring up a container-local Postgres and give it the credentials the default
# DATABASE_URL above names. Deliberately only touches a server this script
# started: a developer's own Postgres is not ours to re-password.
start_postgres() {
    command -v pg_isready >/dev/null 2>&1 || return 1
    [ "$(id -u)" -eq 0 ] || return 1
    echo "validate: no Postgres answering; starting the local one..."
    (service postgresql start || pg_ctlcluster "$(ls /etc/postgresql 2>/dev/null | head -1)" main start) >/dev/null 2>&1
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        pg_isready >/dev/null 2>&1 && break
        sleep 1
    done
    pg_isready >/dev/null 2>&1 || return 1
    su postgres -c "psql -Atc \"ALTER USER postgres PASSWORD 'password'\"" >/dev/null 2>&1
    su postgres -c "psql -Atc 'CREATE DATABASE test_db'" >/dev/null 2>&1
    postgres_ready
}

start_redis() {
    command -v redis-server >/dev/null 2>&1 || return 1
    echo "validate: no Redis answering; starting the local one..."
    # --dir /tmp because a daemonised redis snapshots into its working directory,
    # and an untracked dump.rdb in the repo root is the kind of thing that ends up
    # in a commit.
    redis-server --daemonize yes --dir /tmp >/dev/null 2>&1
    for _ in 1 2 3 4 5; do
        redis_ready && return 0
        sleep 1
    done
    return 1
}

if [ "$MODE" != nodb ] && ! postgres_ready && [ "$START_SERVICES" -eq 1 ]; then
    start_postgres >/dev/null 2>&1 || true
fi
if ! redis_ready && [ "$START_SERVICES" -eq 1 ]; then
    start_redis >/dev/null 2>&1 || true
fi

HAVE_DB=0
postgres_ready && HAVE_DB=1
HAVE_REDIS=0
redis_ready && HAVE_REDIS=1

USE_DB=0
case "$MODE" in
    full)
        USE_DB=1
        if [ "$HAVE_DB" -eq 0 ]; then
            echo "validate: --full needs a Postgres at DATABASE_URL and there is none." >&2
            exit 2
        fi
        ;;
    nodb) USE_DB=0 ;;
    auto) USE_DB=$HAVE_DB ;;
esac

echo "=============================================================="
if [ "$USE_DB" -eq 1 ]; then
    echo " validate: full mode (clippy, tests, sqlx cache)"
    [ "$HAVE_REDIS" -eq 1 ] || echo "   No Redis: the SSE and Gemini-budget tests will print SKIP and pass."
else
    echo " validate: no-database mode (clippy and the constraints only)"
    if [ "$MODE" = auto ]; then
        echo "   Nothing answered at DATABASE_URL, so the ~190 #[sqlx::test] cases cannot"
        echo "   run. On a container: ./validate.sh again as root, or start Postgres by"
        echo "   hand. On a laptop: make dev, or point DATABASE_URL at any local server."
    fi
fi
echo "=============================================================="
echo

STATUS=0
TESTS_RAN=0
SQLX_CHECKED=0

run() {
    local label=$1; shift
    echo "--- $label ---"
    if "$@"; then
        echo
        return 0
    fi
    STATUS=1
    echo
    return 1
}

# Exactly CI's command. Note the absence of --all-targets: CI does not lint the
# test code, and the test code does not currently pass clippy with it, so adding
# it here would fail a change that CI would have taken.
run "clippy (production features)" cargo clippy -- -D warnings

# The configuration developers actually run. It compiles code the line above does
# not -- src/auth/dev_bypass.rs and everything cfg'd on the feature.
run "clippy (dev-auth)" cargo clippy --features dev-auth -- -D warnings

if [ "$USE_DB" -eq 1 ]; then
    # The check that stops a stale .sqlx/ descriptor reaching production: every
    # other step reads the committed descriptors, so a query whose shape changed
    # compiles green everywhere until something talks to real Postgres. This is
    # the only step with SQLX_OFFLINE unset, which is the whole point.
    echo "--- sqlx offline cache ---"
    if command -v cargo-sqlx >/dev/null 2>&1 || command -v sqlx >/dev/null 2>&1; then
        if SQLX_OFFLINE=false cargo sqlx migrate run >/dev/null \
            && SQLX_OFFLINE=false cargo sqlx prepare --check -- --tests; then
            SQLX_CHECKED=1
            echo "ok: the committed .sqlx/ descriptors match the queries in this branch"
        else
            STATUS=1
            echo "The committed .sqlx/ cache does not match this branch's queries."
            echo "Run 'make prepare' against a migrated database and commit .sqlx/."
        fi
    else
        echo "skipped: no sqlx-cli here. Install it with"
        echo "  cargo install sqlx-cli --locked --version ^0.8 --no-default-features --features rustls,postgres"
        echo "CI runs this check regardless, so a stale .sqlx/ still fails there."
    fi
    echo

    # Both halves of the feature matrix, because each compiles tests the other
    # does not: `make test` is the production shape and proves a shipped binary
    # rejects `mock.` tokens; `make test-dev-auth` proves the bypass developers
    # rely on still works.
    run "tests (production features)" make test && TESTS_RAN=1
    run "tests (dev-auth)" make test-dev-auth || true
fi

echo "--- hard constraints (CLAUDE.md) ---"
./scripts/check_constraints.sh || STATUS=1
echo

echo "=============================================================="
if [ "$STATUS" -eq 0 ]; then
    echo " validate: PASSED"
else
    echo " validate: FAILED"
fi
if [ "$USE_DB" -eq 0 ] || [ "$TESTS_RAN" -eq 0 ] || [ "$SQLX_CHECKED" -eq 0 ] || [ "$HAVE_REDIS" -eq 0 ]; then
    echo
    echo " Not checked here:"
    [ "$TESTS_RAN" -eq 1 ] || echo "   - the test suite (needs a Postgres at DATABASE_URL)"
    [ "$SQLX_CHECKED" -eq 1 ] || echo "   - whether .sqlx/ still matches the queries (needs sqlx-cli and a database)"
    [ "$HAVE_REDIS" -eq 1 ] || echo "   - the SSE fan-out and Gemini budget tests (no Redis; they skip silently)"
    echo "   - the release build and the image (only the Dockerfile and CI build those)"
fi
echo "=============================================================="
exit "$STATUS"
