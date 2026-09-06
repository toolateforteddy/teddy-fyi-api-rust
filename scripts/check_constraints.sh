#!/usr/bin/env bash
# Check the five hard constraints from CLAUDE.md.
#
# These are the rules no compiler enforces. `cargo clippy` does not know that a
# `dev-auth` binary in production is total impersonation, that editing a migration
# that has already run stops the next rollout from booting, or that a log line
# carrying a config value is a copy of user data no erasure path can reach. Each
# has a consequence — a security hole, a failed deploy, an un-deletable copy of a
# child's drawing — that a lint warning does not.
#
# The checks are deliberately textual: no toolchain, no database, no network, so
# they run anywhere and in every validate.sh mode, including one where the Rust
# build cannot. That makes them a tripwire, not a proof — they catch the obvious
# regression, not a determined one.

set -uo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.." || exit 1

FAILED=0
CHECKS=0

pass() { printf '  ok    %s\n' "$1"; }
fail() {
    printf '  FAIL  %s\n' "$1"
    shift
    for line in "$@"; do printf '        %s\n' "$line"; done
    FAILED=$((FAILED + 1))
}

# Findings are multi-line grep output. Kept out of fail()'s argument list so the
# shell cannot word-split a match into one word per line.
findings() { printf '%s\n' "$1" | sed 's/^/        /'; }

# --- 1. The `mock.` login bypass never ships ----------------------------------
#
# `src/auth/dev_bypass.rs` accepts a token beginning `mock.` and mints a session
# for whatever user_id the body names. It is gated at compile time precisely so
# that no environment variable can turn it on in a shipped binary; the gate is
# only worth anything while `dev-auth` stays off the default feature list and out
# of every path that builds or runs the production image.
CHECKS=$((CHECKS + 1))
DEFAULT_FEATURES=$(sed -n '/^\[features\]/,/^\[/p' Cargo.toml | grep -E '^default *=' || true)
if printf '%s' "$DEFAULT_FEATURES" | grep -q 'dev-auth'; then
    fail "dev-auth is on the default feature list" \
         "That puts the mock. login bypass into cargo build --release, which is what" \
         "the Dockerfile runs. See CLAUDE.md constraint 1 and src/auth/dev_bypass.rs."
elif [ -z "$DEFAULT_FEATURES" ]; then
    fail "Cargo.toml has no [features] default line" \
         "The bypass is gated by dev-auth being absent from the default features." \
         "An unstated default is not a gate anybody can check. See CLAUDE.md constraint 1."
else
    pass "dev-auth is not a default cargo feature"
fi

CHECKS=$((CHECKS + 1))
SHIPPED_DEV_AUTH=$(grep -rn 'dev-auth\|dev_auth' Dockerfile k8s .github/workflows/deploy.yml 2>/dev/null || true)
if [ -z "$SHIPPED_DEV_AUTH" ]; then
    pass "no shipping path names dev-auth"
else
    fail "dev-auth appears in a path that builds or runs production" \
         "The Dockerfile, the deploy workflow and k8s/ must never name the feature." \
         "See CLAUDE.md constraint 1." ""
    findings "$SHIPPED_DEV_AUTH"
fi

# --- 2. No mod.rs, and module entry files stay declarative --------------------
#
# The repo's module layout rule (README, AGENTS.md): a parent declares its
# children from a sibling file, so `routes.rs` sits next to `routes/`. An entry
# file — one with a directory of the same name beside it — carries declarations
# only. `#[cfg(test)] mod tests;` is a declaration; a test *body* is not.
CHECKS=$((CHECKS + 1))
MOD_RS=$(find src -name 'mod.rs' 2>/dev/null || true)
if [ -z "$MOD_RS" ]; then
    pass "no mod.rs anywhere in src/"
else
    fail "a mod.rs was added" \
         "Declare the module from its sibling file instead: routes.rs beside routes/." \
         "See CLAUDE.md constraint 2." ""
    findings "$MOD_RS"
fi

CHECKS=$((CHECKS + 1))
ENTRY_TESTS=""
while IFS= read -r dir; do
    entry="$dir.rs"
    [ -f "$entry" ] || continue
    found=$(grep -nE '^[[:space:]]*(pub )?mod tests \{|#\[(test|tokio::test|sqlx::test)\]' "$entry" 2>/dev/null || true)
    [ -n "$found" ] && ENTRY_TESTS="$ENTRY_TESTS$(printf '%s\n' "$found" | sed "s|^|$entry:|")
"
done < <(find src -mindepth 1 -type d 2>/dev/null)
ENTRY_TESTS=$(printf '%s' "$ENTRY_TESTS" | sed '/^$/d')
if [ -z "$ENTRY_TESTS" ]; then
    pass "module entry files carry declarations, not test bodies"
else
    fail "a test body lives in a module entry file" \
         "An entry file (X.rs with an X/ directory beside it) declares its children and" \
         "nothing else. Put the tests in X/tests.rs and declare it. See CLAUDE.md" \
         "constraint 2." ""
    findings "$ENTRY_TESTS"
fi

# --- 3. A committed migration is immutable ------------------------------------
#
# db::init_postgres runs sqlx::migrate! on every boot, and sqlx checksums each
# migration it has already applied. Editing one that has run in production means
# the next rollout refuses to start — the deploy fails after the image is pushed,
# with the old pods already terminating. Add a new migration instead.
#
# Needs a fetched origin/main to compare against; skipped rather than failed when
# there is none, since a shallow or offline checkout is not a violation.
CHECKS=$((CHECKS + 1))
BASE=$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)
[ -n "$BASE" ] || BASE=origin/main
if ! git rev-parse --verify --quiet "$BASE" >/dev/null 2>&1; then
    pass "committed migrations unchanged (skipped: no $BASE to compare against)"
else
    TOUCHED=$(git diff --name-status "$BASE...HEAD" -- migrations 2>/dev/null \
        | grep -vE '^A' || true)
    if [ -z "$TOUCHED" ]; then
        pass "no committed migration was edited, renamed or deleted"
    else
        fail "a migration that already exists on $BASE was changed" \
             "sqlx checksums applied migrations, so this fails the next boot in production" \
             "rather than here. Add a new timestamped migration instead. See CLAUDE.md" \
             "constraint 3." ""
        findings "$TOUCHED"
    fi
fi

# --- 4. Secrets never appear in the repository --------------------------------
#
# Values live in GCP Secret Manager and reach the pod through the
# SecretProviderClass; what lives in k8s/ is the wiring. A literal in a manifest
# is a secret published to everyone who can read the repo, and rotating it means
# a commit.
CHECKS=$((CHECKS + 1))
K8S_SECRETS=$(grep -nE '^[[:space:]]*(kind:[[:space:]]*Secret[[:space:]]*$|stringData:)' k8s/*.yaml 2>/dev/null || true)
if [ -z "$K8S_SECRETS" ]; then
    pass "k8s/ declares no Secret body of its own"
else
    fail "a Secret literal appeared in k8s/" \
         "Add the value to GCP Secret Manager and wire it: a parameters.secrets entry, a" \
         "secretObjects.data mapping, and a container env.valueFrom.secretKeyRef. See" \
         "CLAUDE.md constraint 4 and AGENTS.md." ""
    findings "$K8S_SECRETS"
fi

CHECKS=$((CHECKS + 1))
# .env is gitignored; .env.example is the template and must stay one. Real keys
# have shapes: Google's start AIza, service-account JSON carries a PEM.
LEAKED=$(grep -nE '(AIza[0-9A-Za-z_-]{10,}|-----BEGIN [A-Z ]*PRIVATE KEY-----)' \
    .env.example k8s/*.yaml Makefile Dockerfile 2>/dev/null || true)
if [ -z "$LEAKED" ]; then
    pass "no API key or private key material in the tracked config files"
else
    fail "something shaped like a live credential is committed" \
         "See CLAUDE.md constraint 4. If it is real, it is now burned: rotate it, do not" \
         "just delete the line." ""
    findings "$LEAKED"
fi

# --- 5. User data never reaches the logs --------------------------------------
#
# Cloud Logging is reachable by neither DELETE /api/user/data nor
# jobs::reap_stale_users, so a log line carrying a request body or a config value
# is a copy of user data that no erasure path can ever delete. The guard is a test
# suite that asserts on emitted tracing events; this only checks the guard is
# still wired in, because deleting it is the cheap way to make it stop failing.
CHECKS=$((CHECKS + 1))
if [ -f src/routes/sync/tests/log_hygiene.rs ] \
    && grep -q '^mod log_hygiene;' src/routes/sync/tests.rs 2>/dev/null; then
    pass "the log-hygiene test module is present and declared"
else
    fail "the log-hygiene guard is missing or no longer declared" \
         "src/routes/sync/tests/log_hygiene.rs asserts on what the sync path writes to" \
         "the logs, and src/routes/sync/tests.rs must declare it or it never runs." \
         "See CLAUDE.md constraint 5."
fi

# A second tripwire for the same constraint, over the whole tree rather than the
# sync path: no tracing call site may name a user by the raw identifier. The
# emitted-event tests cover the branches they exercise; this covers the call site
# nobody wrote a test for, which is how `refresh_handler` came to log the raw
# Google subject on fourteen lines with the sync guard green the whole time.
# Textual and therefore fallible -- it knows `user_id = %x` and "for user {}", not
# a raw id reached by some other spelling -- but those two are the shapes the
# repo actually wrote. `dev_bypass.rs` is exempt: constraint 1 keeps that file's
# `dev-auth` gate out of every shipped binary, so its warning cannot reach Cloud
# Logging, and it is the one line where a local developer needs the id it was
# handed.
CHECKS=$((CHECKS + 1))
RAW_USER_LOGS=$(grep -rn --include='*.rs' \
    -e 'user_id = %' \
    -e 'for user {}' \
    src/ 2>/dev/null \
    | grep -v '^src/auth/dev_bypass.rs:' \
    | grep -v '/tests\?\.rs:' \
    | grep -v '^src/routes/sync/tests/' || true)
if [ -z "$RAW_USER_LOGS" ]; then
    pass "no log call site names a user by the raw identifier"
else
    fail "a log line names a user by the raw identifier" \
         "Cloud Logging is reachable by neither DELETE /api/user/data nor" \
         "jobs::reap_stale_users, so this is a copy of a user identifier that no" \
         "erasure path can delete. Log observability::http::hash_user_id(id, salt) as" \
         "user_hash instead. See CLAUDE.md constraint 5." ""
    findings "$RAW_USER_LOGS"
fi

# --- 6. No manifest names a moving image tag ----------------------------------
#
# Not one of CLAUDE.md's five, but the same shape of failure: nothing lints it and
# the consequence is silent. `k8s/` pins `:IMAGE_TAG_PLACEHOLDER`, which
# deploy.yml rewrites to the commit SHA before it applies. A manifest that names
# `:latest` instead gives up rollback and provenance, and -- once
# scribbleroute/backend is forked from this repo and pushes to the same registry
# path -- lets that fork's merge to main replace the binary running here, with no
# deploy of ours. See split plan section 1.3, and pre-split item 6.
CHECKS=$((CHECKS + 1))
MOVING_TAGS=$(grep -nE '^[[:space:]]*image:.*teddy-fyi-api-rust:(latest|main|stable)[[:space:]]*$' \
    k8s/*.yaml 2>/dev/null || true)
if [ -z "$MOVING_TAGS" ]; then
    pass "no k8s manifest pins a moving image tag"
else
    fail "a manifest names a moving image tag" \
         "Use gcr.io/melodic-sunbeam-164916/teddy-fyi-api-rust:IMAGE_TAG_PLACEHOLDER;" \
         ".github/workflows/deploy.yml substitutes the commit SHA before applying." ""
    findings "$MOVING_TAGS"
fi

echo
if [ "$FAILED" -eq 0 ]; then
    echo "constraints: $CHECKS checks passed"
else
    echo "constraints: $FAILED of $CHECKS checks FAILED" >&2
fi
exit $((FAILED > 0))
