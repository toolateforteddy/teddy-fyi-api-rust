# Dev Roadmap: teddy-fyi-api-rust (verified 2026-08-28)

Everything below was executed and verified on macOS (darwin 24.6.0), not inferred.
Companion docs: [AGENTS.md](../AGENTS.md), [README.md](../README.md),
[2026-07-10_project_context.md](2026-07-10_project_context.md).

---

## 1. TL;DR — get productive in 60 seconds

```bash
docker start local-postgres teddy-redis-dev
sqlx migrate run   # DATABASE_URL=postgresql://postgres:postgres@localhost:5432/neondb
make test
```

`make test` passes: **60 tests, 0 failures, ~8s**. `cargo clippy -- -D warnings`
(the CI gate) is clean.

You do **not** need Neon, `neonctl`, npm, or network access for the normal
build/test loop. `make dev` (the Neon-branching path) is the *other* workflow — see §4.

---

## 2. The two Docker containers

Both already exist on this machine; they only need `docker start`.

| Container | Image | Port | Who creates it | What breaks without it |
| :-- | :-- | :-- | :-- | :-- |
| `local-postgres` | `postgres:16` | 5432 | **manual** (not scripted anywhere) | `cargo test`, `cargo sqlx prepare`, `cargo run` |
| `teddy-redis-dev` | `valkey/valkey:7.2` | 6379 | `scripts/dev.sh` | nothing hard-fails — see §5 |

`local-postgres` is env-configured as `POSTGRES_USER=postgres`,
`POSTGRES_PASSWORD=postgres`, `POSTGRES_DB=neondb` — which is exactly the
Makefile's default `DATABASE_URL`. That is not a coincidence; it is the whole
local-DB story, and it is written down nowhere else in the repo. Recreate with:

```bash
docker run -d --name local-postgres -p 5432:5432 -e POSTGRES_USER=postgres -e POSTGRES_PASSWORD=postgres -e POSTGRES_DB=neondb postgres:16
```

Recreate Redis (mirrors `scripts/dev.sh`):

```bash
docker run -d --name teddy-redis-dev -p 6379:6379 valkey/valkey:7.2
```

---

## 3. How the SQLx compile-time/runtime split actually works

This is the single biggest source of confusion in the repo. There are **two
independent** database dependencies:

1. **Compile time.** `sqlx::query!` / `query_as!` verify SQL against a live
   database *unless* `SQLX_OFFLINE=true`, in which case they read the cached
   JSON in `.sqlx/`.
2. **Run time.** All 47 `#[sqlx::test]` tests spin up a throwaway database from
   `/migrations` and hand the test a `PgPool`. This needs a live server **even
   when `SQLX_OFFLINE=true`**.

So CI (`.github/workflows/deploy.yml`) correctly sets *both* `SQLX_OFFLINE=true`
*and* a live `DATABASE_URL` against a `postgres:15` service. Do the same locally.

### `.env` is read by the compiler, and it lies

`sqlx` and `sqlx-cli` load `.env` via dotenvy, which does **not** override
variables already set in the shell. The committed-local `.env` currently has
`SQLX_OFFLINE=false` and a `DATABASE_URL` pointing at a Neon dev branch created
in late June — `scripts/dev.sh` stamps those branches with a 24-hour expiry, so
it is long dead. **Always pass `DATABASE_URL` and `SQLX_OFFLINE` explicitly** and
they will win over `.env`. The Makefile already does this for `DATABASE_URL`.

### Working commands

```bash
# fast offline compile — no DB needed at all
SQLX_OFFLINE=true cargo check --tests

# full suite (Makefile default DATABASE_URL is already correct)
make test

# regenerate the .sqlx cache — note the `-- --tests`
make prepare
```

### `cargo sqlx prepare` — the two traps

* **`-- --tests` is mandatory.** Bare `cargo sqlx prepare` only walks the binary
  target and silently *deletes* every cache entry that exists solely for test
  code — 68 of the 159 files. Offline test builds then fail with
  `SQLX_OFFLINE=true but there is no cached data for this query`. `make prepare`
  gets this right; typing the command by hand usually doesn't.
* **`prepare` runs against your live DB, so migrate it first.** If
  `local-postgres` is behind on migrations, `query_as!(Session, "SELECT * ...")`
  resolves against the stale table and you get a *cascading, misleading* Rust
  error — `E0063: missing fields old_refresh_token_hash and rotated_at in
  initializer of Session` — rather than an obvious "your DB is stale" message.
  Fix is `sqlx migrate run`, not editing the struct.

The installed `sqlx-cli` is **0.9.0** while the crate is **0.8**. Despite the
mismatch, `cargo sqlx prepare -- --tests` produces the identical on-disk format
and round-trips cleanly. Verified, not assumed.

---

## 4. `make dev` is a different workflow

`scripts/dev.sh` ignores `local-postgres` entirely. It provisions a *fresh Neon
branch* cloned from production on every run:

1. `npx neonctl` for the project ID (needs `npx neonctl auth` first).
2. Deletes stale `dev-$USER-*` branches, creates `dev-$USER-$(date +%s)` with a
   24h expiry, cloned from `production`.
3. Appends `statement_cache_size=0&prepared_statement_cache_size=0` for PgBouncer.
4. **Rewrites `.env`** — `DATABASE_URL`, `SQLX_OFFLINE=false`, `REDIS_URL`.
5. Starts `teddy-redis-dev`, runs migrations, `exec cargo watch -x run`.

Reach for it when you need production-shaped data. For everyday work
`local-postgres` is faster and offline. Note step 4: **`make dev` will clobber
your `.env`.**

---

## 5. Redis is optional-by-design, and that hides test coverage

Every Redis call site is `if let Ok(conn) = ...get_multiplexed_tokio_connection()`
— `sync_handler` cache writes, `sync_status_handler` reads, and the fallback
population in `status.rs`. Failures are logged at `warn` and swallowed; the
status endpoint just falls through to the (expensive) DB aggregate query.

The tests inherit that shape: `src/routes/sync/tests/cache.rs` wraps **all** its
Redis assertions in `if let Ok(mut conn)`. Verified by running the suite against
a closed port — all 4 cache tests still report `ok` while asserting nothing.
CI has no Redis service, so **the caching layer is effectively untested in CI**.
Start `teddy-redis-dev` locally if you touch cache logic, or those tests are
green theater. The SSE tests have the same blind spot for a different reason —
they never touch Redis at all (§8).

---

## 6. Architecture map

Module layout rule (enforced by AGENTS.md/README): **no `mod.rs`.** Entry files
like `routes.rs`, `sync.rs`, `grocery.rs` are pure `pub mod` / `pub use`
declarations; siblings live in the matching directory. No logic, no tests in them.

```
main.rs            router, CORS (locked to https://teddy.fyi), pool, migrate-on-boot, graceful shutdown
state.rs           AppState: pg pool, redis client, google client ids, jwt secret, gemini key, cookie domain
auth/              middleware (require_auth), handlers (login/refresh/logout), tokens (JWT+argon2), models
routes/sync/       handler.rs  ← the endpoint
                   types.rs    ← wire contract + AppError + AppJson extractor
                   status.rs   ← GET /api/sync/status
                   stream.rs   ← GET /api/sync/stream (SSE)
                   publisher.rs← Redis Pub/Sub fan-out behind the SSE stream
                   models.rs   ← ORPHANED, never declared — see §8
                   grocery/    lists, members, stores, categories, items, item_store_info, remote_mutations
                   todo/       lists, items, remote_mutations
                   config.rs   } ScribbleKeep / ScribbleBox / ScribbleKeepCloud scopes
                   drawing.rs  }
routes/ai/         gemini.rs (JSON-schema-constrained calls), handlers, service (icon allowlist)
routes/lists/      invite / join (8-char single-use codes, 24h expiry)
dao/, models/      Config/Drawing DAO + typed models — see "dead code" below
```

### Routes

| Method | Path | Auth |
| :-- | :-- | :-- |
| GET | `/hello`, `/hellov2`, `/healthcheck` | none |
| POST | `/auth/login`, `/auth/refresh`, `/auth/logout` | none (self-authenticating) |
| POST | `/api/sync` | required |
| GET | `/api/sync/status` | required |
| GET | `/api/sync/stream`, `/api/v1/sync/stream` | required |
| POST | `/api/categorize`, `/api/assign-icon` | required |
| POST | `/api/lists/invite`, `/api/lists/join` | required |
| GET | `/api/hc`, `/api/ready` | required |

`require_auth` demands **both** an `X-Client-UUID` header and a token (Bearer,
or `access_token` cookie), and rejects with 403 if the header disagrees with the
JWT's `client_uuid` claim. Missing header → **400**, missing token → **401**.
Note `/api/hc` and `/api/ready` sit *behind* auth — the unauthenticated liveness
probe is the top-level `/healthcheck`.

### How `sync_handler` is shaped

Three `tokio::try_join!` futures — **todo**, **grocery**, **config/drawing** —
each opening its own transaction, gated on `SyncScope`. Scopes are disjoint in
practice: `All`/`Todo`/`Grocery` never overlap the three Scribble scopes.

Per domain the pattern is identical: `process_*_changes` (upload, permission
check, MVCC-with-LWW-fallback, version bump) writes into shared `success_ids` /
`upload_status` / `remote_*` vectors, then `fetch_remote_*_mutations` pulls the
download delta, then the two are merged **de-duplicated by id, uploads winning**.
Echo prevention is `updated_by_client != $client_id`, bypassed on initial sync.

After commit, cache keys `user:{id}:last_update:{Scope}` are written with a
86400s TTL — including for every collaborator on a touched grocery list, which
is what that large `affected_grocery_users` UNION query in `handler.rs` computes.

### Data model quirks worth knowing

* IDs are **TEXT**, not native `uuid` — grocery ids were migrated *away* from
  `SERIAL` to TEXT in `20260625000000`. `context/LEGACY_DB_MIGRATION.md`
  describes a move to native `UUID`/enum types that has **not** been executed.
* Column naming is mixed: Room-style camelCase in quotes (`"listId"`,
  `"isCompleted"`, `"userId"`) alongside snake_case (`updated_at`, `is_deleted`,
  `sync_state`, `updated_by_client`). Quote the camelCase ones or Postgres folds them.
* `configs` / `drawings` are the exception — real `UUID` PKs and a real
  `sync_state` **enum**. Non-UUID user ids are coerced by
  `parse_or_hash_uuid()` (parse, else deterministic UUIDv5 over DNS namespace).
* `grocery_item_store_info` has a **composite** PK `(groceryItemId, storeId)`
  and no `id` column; the wire `id` is synthesized as `"{item}-{store}"` unless
  the client supplies its own.

---

## 7. Current working-tree state (uncommitted)

An in-flight change adds a server-computed `list_id` to `GroceryItemStoreInfoData`
so clients can route store-info rows to a list without a second lookup —
resolved from the store on the download path, from the parent grocery item on
the echo path. Touches `types.rs`, `grocery/remote_mutations.rs`,
`grocery/grocery_item_store_info.rs`, `tests/grocery.rs`.

I regenerated `.sqlx` with `make prepare`, which restored the 68 files a bare
`cargo sqlx prepare` had deleted. `.sqlx` is internally consistent at 159 files:
one query deleted, one added, matching the diff.

Both remain **uncommitted and deliberately out of this branch** — splitting the
`.sqlx` delta from the source change that motivates it would leave a query with
no cache entry and break the offline build, and therefore the deploy.

### Both `janitor/*` branches have since merged (PRs #1, #2)

They brought `.github/workflows/CI.yml`, `code-janitor.yml`, a lint sweep, and
an idempotent store-info DELETE — the old `RETURNING version` + `.fetch_one()`
that aborted the whole grocery transaction on an already-deleted row is fixed.

One of the two didn't fully land: `routes/sync/models.rs` is on disk but **no
`pub mod models;` declares it**, so it is not compiled — see §8.

### `main` was red for 26 days

`a5e8c2a "Implement the SSE endpoint"` (2026-08-02) was pushed straight to
`main` and its CI run failed. Nothing ran on `main` afterwards except the
scheduled Code Janitor, so it went unnoticed until the next pull request
inherited the breakage through the PR merge commit. Two failures, one masking
the other:

1. `publisher.rs:41` — `E0382 borrow of moved value: channel`. `channel` was
   moved into `conn.publish(...)` and then borrowed by the `tracing::info!`
   on the next line. Fixed by passing `&channel`.
2. `stream.rs:85` — `clippy::collapsible_match`, fatal under CI's
   `cargo clippy -- -D warnings`. The `if` inside the match arm is now a match
   guard. Behaviour is identical: a failing guard falls through to `_ => {}`.

A third failure was hiding behind those two — see below.

Worth knowing that `CI.yml` runs on `pull_request` as well as `push`, so a PR
is checked against its *merge* with `main` — a doc-only branch can go red for
reasons that have nothing to do with it.

### CI's Rust toolchain is unpinned, and that bites

There is no `rust-toolchain.toml`, and `CI.yml` uses
`actions-rust-lang/setup-rust-toolchain@v1` with no `toolchain:` key, so CI
installs **whatever stable is current on the day**. It is on **1.98.0**.

That is a real failure mode, not a footnote: `clippy::result_large_err` fires
under 1.98 on `require_auth` and `refresh_handler` — both
`-> Result<Response, Response>` — and does not fire under 1.95. **A new stable
release can turn this repo red with no code change at all.**

There is no boxing fix for those two: axum requires `IntoResponse` on *both*
variants of the returned `Result`, and `Box<Response>` does not implement it. So
they carry a narrow `#[allow(clippy::result_large_err)]` with a comment. A
crate-wide `[lints.clippy]` entry in `Cargo.toml` would also work and would stop
each new handler re-tripping it, at the cost of silencing the lint where it
might be genuine.

**If you are debugging a CI clippy failure you cannot reproduce, check your
toolchain first.** Local stable here was 1.95.0 and reported the code clean.
Reproduce CI exactly with:

```bash
rustup toolchain install 1.98.0 --component clippy --profile minimal
SQLX_OFFLINE=true cargo +1.98.0 clippy -- -D warnings
```

Pinning CI (a `rust-toolchain.toml`, or a `toolchain:` key) would trade
surprise breakage for explicit upgrades. That is a policy call, so it is
flagged here rather than made.

---

## 8. Things that are wrong or stale (not yet fixed)

* **`login_handler` trusts the client's `user_id`.** The Google token is
  validated and its `aud` checked against the allowed client-id set, but the
  session is then created for `payload.user_id` from the request body rather
  than the verified Google `sub`. A caller with any valid token for an allowed
  audience can request a session for an arbitrary user id. Worth a hard look.
* **Dev auth bypass is gated only on `COOKIE_DOMAIN=""`.** A `google_auth_token`
  starting with `mock.` skips Google verification entirely when the cookie
  domain is empty. Safe by default (unset → `.teddy.fyi`), but an explicit empty
  `COOKIE_DOMAIN` in a deployed environment disables authentication.
* **AI model ids disagree.** `ai/handlers.rs` uses `gemini-2.5-flash-lite`;
  `ai/service.rs` uses `gemini-3.1-flash-lite`. README documents 2.5. At most
  one is right.
* **README's `/api/categorize` response is wrong** — documented as
  `{"category": ...}`, actual field is `selected_category`.
* **`2026-07-10_project_context.md` describes the old breach policy.** It says
  an invalid refresh token deletes *all* the user's sessions; commit 3191de9
  ("Narrower breach mitigation") narrowed every path to deleting the single
  offending session.
* **`routes/sync/models.rs` is an orphan.** Nothing declares `pub mod models;`,
  so the file is never compiled: its `SyncScope::includes()` helper is
  unreachable, the `==` chains in `handler.rs` it was meant to replace are still
  there, and its own unit test never runs (`cargo test sync_scope` matches 0
  tests). It also declares a **second, incompatible** `SyncScope` — variants
  `Habit`/`ScribbleNote` where the live one in `types.rs` has
  `ScribbleKeepCloud`, and `snake_case` serde where the live one is
  `SCREAMING_SNAKE_CASE`. Wiring it in as-is would not be a no-op; reconcile the
  two enums first.
* **The SSE path has no behavioural test.** All 5 tests in `tests/stream.rs` are
  pure serialization and header assertions — the Pub/Sub plumbing and the
  echo-filtering guard in `stream.rs` are never exercised. Same shape of gap as
  the cache tests above.
* **Dead code**, invisible to rustc because the `pub use` glob re-exports in the
  module entry files suppress the lint: `dao::{ConfigDao, DrawingDao}` and
  `models::{Config, Drawing}` are referenced only by their own tests — the
  Scribble sync path uses raw `sqlx` in `routes/sync/config.rs` / `drawing.rs`.
  `routes/sync/remote_mutations.rs::fetch_remote_mutations` is likewise
  superseded by `handler.rs` calling the per-domain fetchers directly.
* **`categorize_item_handler` scopes categories to `"userId" = $1` only**, so a
  user gets no suggestions from categories owned by a shared list they belong to.
* **Clippy's CI gate only covers the binary.** `CI.yml` runs
  `cargo clippy -- -D warnings`, which checks the bin target only — that is
  clean. `cargo clippy --all-targets` still reports **30** warnings in test
  code (25 of them `needless_borrow`), ungated. Zero rustc warnings either way.

---

## 9. CI and deploy

Three workflows:

* `CI.yml` — on push **and** `pull_request`. `cargo clippy -- -D warnings`, then
  `make test` against a `postgres:15` service. **No Redis service**, so §5 applies.
* `code-janitor.yml` — scheduled; opens the `janitor/*` cleanup PRs.
* `deploy.yml` — on push to `main`/`master`.

Push to `main`/`master` → `.github/workflows/deploy.yml`: runs `make test`
against a `postgres:15` service (no Redis), then cargo-chef Docker build with
`SQLX_OFFLINE=true`, push to `gcr.io/melodic-sunbeam-164916/teddy-fyi-api-rust`,
then `kubectl rollout restart deployment/api-rust-dep` on GKE cluster `prod`
(us-central1-a). The container serves on `PORT` (default 8080; 3000 outside Docker).

Because the image builds with `SQLX_OFFLINE=true`, **a stale `.sqlx` breaks the
deploy, not just local tests.** Run `make prepare` and commit `.sqlx` whenever
you touch SQL.

---

## 10. Verified local smoke test

```bash
SQLX_OFFLINE=true DATABASE_URL="postgresql://postgres:postgres@localhost:5432/neondb" \
JWT_SECRET=dev_secret_key_123 GEMINI_API_KEY=dev REDIS_URL="redis://127.0.0.1:6379" \
COOKIE_DOMAIN="" PORT=3999 cargo run
```

`COOKIE_DOMAIN=""` unlocks the `mock.` login bypass, so you can mint a real JWT
with no Google round-trip:

```bash
curl -s -X POST localhost:3999/auth/login -H 'Content-Type: application/json' -d '{"user_id":"local-dev-user","client_uuid":"client-1","google_auth_token":"mock.dev"}'
```

Then every authenticated call needs both headers:

```bash
curl -s -H "Authorization: Bearer $TOKEN" -H 'X-Client-UUID: client-1' localhost:3999/api/sync/status?scope=ALL
```

Confirmed responses: `/healthcheck` → `OK`; `/api/hc` bare → 400 missing header;
with header, no token → 401; with both → `OK`; `/api/sync/status` →
`{"needs_sync":true,"latest_version":"1970-01-01T00:00:00Z"}`; empty `POST
/api/sync` → `{"server_timestamp":"..."}` (all other fields are
`skip_serializing_if = "Vec::is_empty"`, so an empty response really is that bare).
