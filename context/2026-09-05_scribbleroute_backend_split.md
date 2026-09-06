# Splitting the ScribbleRoute backend out of `teddy-fyi-api-rust`

*Written 2026-09-05, before any of it is built. Status: **plan, not yet started.***

*Revised the same day: the fork no longer comes first. §3 explains why, and what changed.*

This service currently answers for two products from one process, one image, one database and
one Redis:

* **ScribbleRoute** — ScribbleKeep / ScribbleBox on Fire tablets, launched to internal testing,
  with real external users, a published privacy policy at <https://scribbleroute.com/privacy>,
  and its own hostname `api.scribbleroute.com`.
* **teddy.fyi** — the personal grocery and todo apps, one household, no external users, no
  published policy, hostname `api-rust.teddy.fyi`.

The goal is to separate them. This note records *what is actually entangled* (read out of the
code, not assumed), the options considered, the option chosen, and the ordered steps with their
rollbacks.

The identity re-key that rides along with the data cut has its own note:
`context/2026-09-05_identity_model.md`.

---

## 1. What is actually shared today

### 1.1 The seam that is already clean

`SyncScope` (`src/routes/sync/types.rs`) partitions the sync endpoint by product, and the handler
dispatches on it into three independent futures with independent transactions:

| Scope | Product | Tables |
|---|---|---|
| `Todo`, `Grocery` | teddy.fyi | `todo_lists`, `todo_items`, `grocery_lists`, `grocery_list_members`, `grocery_items`, `grocery_item_store_info`, `stores`, `categories`, `list_invites`, `list_join_failures` |
| `ScribbleBox`, `ScribbleKeep`, `ScribbleKeepCloud` | ScribbleRoute | `configs`, `drawings`, `devices` |

Nothing in the ScribbleRoute branch of `sync_handler` reads a grocery or todo table, and nothing
in the grocery/todo branch reads `configs` or `drawings`. The AI routes (`/api/categorize`,
`/api/assign-icon`) and the list-invite routes (`/api/lists/*`) are teddy.fyi-only.
`/api/devices/*` is ScribbleRoute-only. So the *code* cut is close to mechanical: delete whole
modules, delete whole route lines.

**`SyncScope::All` is already a teddy.fyi-only concept.** It runs the todo and grocery futures;
the ScribbleRoute branch is entered only by the three `Scribble*` variants. It is also the
default when a client omits `scope` (`handler.rs:30`). So every ScribbleRoute client must already
be sending an explicit scope on every request — an undocumented load-bearing assumption, and the
first thing §5's product profile turns into an assertion.

### 1.2 The couplings that are not clean

**Auth is one tier, not two.** Both products validate against one `JWT_SECRET`, one audience set
(`SCRIBBLEROUTE_API_CLIENT_ID` sits in the same `SINGLE_ID_ENV_VARS` list as `GOOGLE_CLIENT_ID`
and `GOOGLE_CLIENT_ID_GROCERY_WEB`, see `src/auth/client_ids.rs`), and one `sessions` table keyed
`(user_id, client_uuid)`. A token minted for a tablet is structurally acceptable to the grocery
backend and vice versa; today they are the same backend, so this has never mattered. It starts
mattering the moment there are two.

**`users` is shared and cannot be partitioned in SQL.** `users.id` is the raw Google auth subject
(TEXT). `configs.user_id`, `drawings.user_id` and `devices.user_id` are UUIDs derived by
`parse_or_hash_uuid(sub)` — a *one-way* hash for non-UUID subjects. This is the subject of
`context/2026-09-05_user_identity_derivation.md`, and its consequence here is concrete: **you
cannot write a SQL query that selects "the `users` rows belonging to ScribbleRoute accounts."**
`find_stale_users` in `src/jobs/reap_stale_users.rs` already works around this by doing the join
in Rust. Any data-tier split has to do the same.

**Deletion is a single function spanning both universes.** `delete_user_data`
(`src/routes/user/deletion.rs`) erases todo, grocery, configs, drawings, devices, device
authorizations, sessions and the user row in one transaction. `DELETE /api/user/data` and the
retention reaper both go through it. A ScribbleRoute user exercising their delete right today
deletes grocery rows in the same transaction — which is exactly the entanglement that prompted
this split, and the first thing the product profile fixes.

**The retention reaper runs both products' cleanup in one process.** `run_reaper` sweeps device
authorizations and stale accounts (ScribbleRoute) *and* expired list invites (teddy.fyi) in one
CronJob. Its eligibility rule — only accounts with a `devices` row — exists solely because `users`
is shared and no published policy covers the grocery side.

**Redis keys are shared but namespaced by scope.** Keys are `user:{sub}:last_update:{scope}` and
`ai:gemini:calls:user:{id}:{day}`. The scope suffix means there is no *collision* risk today. The
real shared-Redis risks are eviction pressure, a stray `FLUSHDB`, and the SSE pub/sub fanout
channels — not key names.

**`COOKIE_DOMAIN` defaults to `.teddy.fyi` and is not set in the manifests**, so that is the
production value for *both* hostnames. `session_cookie` therefore emits `Domain=.teddy.fyi` on
responses served from `api.scribbleroute.com`, which a browser on `scribbleroute.com/link`
discards. A latent bug, and one the profile fixes for free.

**`CORS_ALLOWED_ORIGINS` is unset in prod**, so the compiled default in `main.rs` (teddy.fyi plus
both spellings of scribbleroute.com) *is* the production value. Both sides need to narrow it.

### 1.3 Two hazards the fork brings, and when they now arrive

Both come from a fork inheriting `.github/workflows/deploy.yml` and `k8s/` verbatim:

1. **The image tag is shared.** `deploy.yml` pushes
   `gcr.io/melodic-sunbeam-164916/teddy-fyi-api-rust:latest`. A fork that merges to `main`
   overwrites the *running* image, and the next `rollout restart` anywhere pulls the fork's
   binary into whatever pulls that tag.
2. **Every k8s resource name is unqualified** (`api-rust-dep`, `api-rust-svc`, `api-rust-ksa`,
   the `SecretProviderClass`, the `BackendConfig`, both ManagedCertificates), and `deploy.yml`
   runs `kubectl apply -f k8s/` then `kubectl rollout restart deployment/api-rust-dep`. Two
   repos applying overlapping names fight over the result.

Under the revised ordering these arrive at **Phase 4**, not on day one, and both are smaller by
then: the ScribbleRoute manifests were renamed in Phase 2 and have been running in production
under their new names ever since, so the fork inherits a proven set rather than an untested
rename. What remains is a single ownership handover, spelled out in Phase 4.

Three cross-repo facts that do not change:

* `site-ingress` lives in the **`teddyfyi` repo**. It references `api-rust-svc` by name and names
  both ManagedCertificates in its `networking.gke.io/managed-certificates` annotation. The
  traffic cutover is a change in *that* repo.
* `cache.yaml`'s NetworkPolicy admits only `podSelector: app: api-rust`. A new pod with a
  different label cannot reach `cache-svc` at all — convenient, since a misconfigured new
  deployment fails closed rather than quietly sharing Redis.
* **The binary migrates on boot.** `db::init_postgres` runs `sqlx::migrate!("./migrations")`
  on every start (`src/db.rs:202`) against whatever `DATABASE_URL` names. An earlier version of
  this note claimed the opposite; `context/2026-09-05_pre_split_changes.md` item 1 caught it and
  worked out the mechanics. It is **not** a race — sqlx takes an exclusive advisory lock — but
  `ignore_missing: false` means a binary meeting a database carrying a migration version it does
  not itself have fails with `VersionMissing`, `init_postgres` returns `Err`, and the `.expect`
  panics. A crashloop, not corruption: the good direction, but it constrains the ordering below
  wherever two migration directories can meet one database.

---

## 2. Options considered

Isolation has four independent axes — compute, database, Redis, and source — and they can be cut
at different times. The central insight of the revision is that **only the last one needs a
fork**.

| | Compute | Postgres | Redis | Source | Migration effort |
|---|---|---|---|---|---|
| **A. One repo, two deployments, product profile** | separate | shared | separate | one | none |
| **B. Fork, shared data tier** | separate | shared | shared | two | none |
| **C. Fork, separate DB + Redis** | separate | separate DB, same Neon project | separate pod | two | one copy + freeze |
| **D. Full isolation** | separate | separate Neon project | separate pod | two | as C, plus new project |
| **E. Cargo workspace, one deployment** | shared | shared | shared | one | refactor |

**A — one image, two deployments, selected by an `APP_PROFILE` env var.** Buys the entire compute
isolation: a panic, a memory leak or a runaway AI call on the grocery side can no longer take
ScribbleRoute down. Needs no fork, no second CI, no data work. It also makes the *seam* testable
before anything is duplicated — see §3. Its limit is that isolation is a runtime property of a
config value, not a structural property of the code.

**B — fork, keep sharing Postgres and Redis.** The original instinct. Everything A gives, plus a
repo whose contents you would not mind an outsider reading, at the cost of duplicating a codebase
before any of the operational work has been proven.

**C — fork, dedicated database in the same Neon project, dedicated Redis.** Deletion becomes
structurally incapable of touching grocery rows; retention becomes a policy over a database that
contains only accounts it covers. The destination.

**D — separate Neon project.** C plus separate credentials and a boundary that survives
ScribbleRoute changing hands. Cheap to reach *from* C later, so not a starting point.

**E — Cargo workspace.** Solves code drift, solves nothing else. Wrong tool: the itch is blast
radius and data.

## 3. Chosen path, and what the revision changed

**A first, then the traffic flip, then the fork, then C.**

```
Phase 1  product profile in this repo          → the seam becomes executable
Phase 2  second deployment + its own Valkey    → compute isolation, no traffic
Phase 3  flip api.scribbleroute.com            → one reversible annotation
Phase 4  fork                                  → a pure refactor, checklist in hand
Phase 5  data cut + identity re-key            → the one freeze
Phase 6  decommission here
```

**What changed and why.** The first version of this plan forked first, on the reasoning that the
repo split was the point. That put the duplication *before* every risky operational step, which
meant each of those steps was taken against a codebase that had never run in production. The
profile inverts it: Phases 2 and 3 need no fork at all, so the two riskiest moves — standing up a
second deployment and moving live traffic — happen with one codebase, one CI and nothing to keep
in sync. If either goes wrong there is one place to fix it.

What the profile buys beyond convenience:

* **It makes the cut line executable.** The Phase 4 deletion list stops being a list somebody
  wrote down and becomes what CI enforces: whatever the ScribbleRoute profile excludes is exactly
  what the fork deletes. Tests asserting the profile's route set become tests asserting the fork's
  contents.
* **It answers questions the fork would otherwise discover in production** — whether any tablet
  relies on the default `All` scope, whether any route you planned to delete is still called.
* **It fixes the deletion entanglement immediately.** Profile-scoped `delete_user_data` can ship
  in days, long before the database is touched. That was the original itch.

**Why the fork still has to precede Phase 5.** The identity re-key changes `users.id` from TEXT to
UUID. If only the ScribbleRoute database is re-keyed while one binary serves both products, that
binary straddles two databases with two different id types — the two-identity bug again, in the
worst possible place. Forking first means the re-key lands in a binary that serves one product
against one database. (The alternative, re-keying both databases in one freeze, is viable — grocery
is one household — but it means doing the identity work for teddy.fyi, which
`context/2026-09-05_identity_model.md` §10 explicitly declines.)

Leave the door open to D: C's schema is the whole database, so moving it to its own Neon project
later is a `pg_dump`/`pg_restore` and a secret, with no code change.

---

## 4. Decisions this plan bakes in

Write these down now; they are the ones that are expensive to reverse.

1. **`APP_PROFILE` is required and fails closed.** No default. An unset or unrecognised value
   panics at startup, the way `assert_startup_config` already refuses to boot without an audience
   allowlist — a rollout failure, not a request failure. The transitional value `both` reproduces
   today's behaviour and exists only until Phase 3; it logs a warning at every boot and is deleted
   in Phase 6.
2. **A person who uses both products becomes two accounts.** Same Google identity, two `users`
   rows in two databases, two `sessions` rows, no shared state. This is the point of the split,
   and the overlap is the author's own household.
3. **`JWT_SECRET` stays identical across the cutover.** Both deployments must accept the tokens
   already in the field, or every tester is logged out at the flip. Rotating it is a separate,
   later, deliberate change on the ScribbleRoute side only.
4. **`sessions` and `device_authorizations` rows get copied** in Phase 5: a `/auth/refresh` that
   401s means a forced re-login on every tablet, and an in-flight pairing that fails means a
   parent retries a code that no longer exists.
5. **The retention reaper's sweeps split by product.** Device authorizations and stale accounts
   are ScribbleRoute; expired list invites are teddy.fyi. The `users`-is-shared eligibility rule
   (only accounts with a `devices` row) survives until Phase 5 and is dropped there.
6. **`cache.yaml` is not duplicated.** The existing Valkey stays teddy.fyi's; ScribbleRoute gets
   a new one under its own name, with a NetworkPolicy selecting the new pod label.
7. **The teddy.fyi database keeps no ScribbleRoute user rows.** Dropping the tables is not enough
   — external testers' `users` and `sessions` rows have to go too, and selecting them requires the
   Rust-side identity join (§1.2).
8. **The identity re-key rides along with the data cut** in Phase 5. `users.id` becomes an opaque
   surrogate UUID and the Google subject becomes an attribute; the design is
   `context/2026-09-05_identity_model.md`. The six-step dual-write migration it would otherwise
   need exists *only* because there is no maintenance window, and Phase 5 is that window.

---

## 5. Steps

Each phase ends in a state that is safe to sit in indefinitely. Do not start the next one until
the current one has been observed healthy for at least a day of real tablet traffic.

### Phase 0 — Measure, change nothing

1. Take a Neon backup / branch of the current database. It is the rollback for everything below
   that touches data.
2. Record row counts for `configs`, `drawings`, `devices`, `device_authorizations`,
   `device_claim_failures`, `sessions`, `users`. They are the check figures for Phase 5.
3. Answer the two questions the Phase 5 re-key depends on, both cheap and both better answered
   now than under a freeze (`context/2026-09-05_identity_model.md` §12):
   * the **orphan audit** — `configs`/`drawings`/`devices` rows whose `user_id` is the hash of no
     surviving `users.id`. Some are expected: those two tables predate `users` by one migration.
     A large count makes the re-key a different conversation.
   * ~~whether the Android and iOS clients read `sub` from **our** JWT or take Google's `sub`
     from the Google ID token.~~ **Answered 2026-09-06: they take Google's `sub`** — the second
     answer, so a client release does precede Phase 5, and the compatibility shim it needs
     outlives client versions rather than tokens. The release is behaviourally a no-op before the
     cutover, so it can and should ship now: its adoption curve, not the freeze, is the long pole
     in front of the Phase 5 re-key. See `context/2026-09-05_identity_model.md` §7.1-7.2.

### Phase 1 — The product profile (one repo, one deployment, no visible change)

The whole phase ships behind `APP_PROFILE=both`, which is today's behaviour, so it can go to
production in pieces.

1. `AppProfile` read once at startup, required, fail-closed (decision 1). Three values:
   `scribbleroute`, `teddy_fyi`, `both`.
2. **Router assembly branches on it.** Under `scribbleroute`: `/api/sync`, `/api/sync/status`,
   `/api/sync/stream`, `/api/devices/*`, `/api/user/data`, `/auth/*`, the health routes. Under
   `teddy_fyi`: the same minus `/api/devices/*`, plus `/api/categorize`, `/api/assign-icon`,
   `/api/lists/*`.
3. **`sync_handler` rejects foreign scopes** — including `All` under `scribbleroute`, per §1.1.
   Log the rejection with the scope and client id: if that line never appears, the assumption in
   §1.1 is confirmed by production rather than by reading.
4. **`delete_user_data` is scoped by profile.** This is the phase's real prize and the answer to
   the original itch; it lands weeks before the database is touched.
5. **The reaper's sweeps are gated by profile** (decision 5), so the CronJob can be pointed at one
   product later without a code change.
6. **Per-profile config defaults**: `CORS_ALLOWED_ORIGINS`, `COOKIE_DOMAIN` (fixing the
   `.teddy.fyi` cookie on `api.scribbleroute.com`), `APP_VERIFICATION_URIS`, and the audience set
   — `SCRIBBLEROUTE_API_CLIENT_ID` under one profile, `GOOGLE_CLIENT_ID_GROCERY_WEB` under the
   other. `GEMINI_API_KEY` becomes required only under `teddy_fyi`/`both`, or the ScribbleRoute
   deployment cannot boot without a key it never uses.
7. **Tests pinning each profile's route set and rejected scopes.** These are the artefact that
   makes Phase 4 mechanical; write them as the specification of the cut, not as coverage.
8. Deploy with `APP_PROFILE=both`. Nothing changes for anyone.

Migrations are unconstrained through Phases 1–3: one repo, one migrations directory, one image.
Both pods run the same set against the same database under the same advisory lock, so §1.3's
`VersionMissing` hazard cannot arise until there are two repos.

### Phase 2 — Second deployment, no traffic

1. New manifests, named for the product: `scribbleroute-api-dep`, `-svc`, `-ksa`,
   `scribbleroute-api-gcp-secrets`, `-be-config`. Same image, same `deploy.yml`, both applied and
   restarted by the one workflow.
2. Its own Valkey: `scribbleroute-cache-dep` / `-svc`, plus a NetworkPolicy selecting the new API
   pod label. Point its `REDIS_URL` there.
3. Env: `APP_PROFILE=scribbleroute`, `COOKIE_DOMAIN=.scribbleroute.com`, the **existing**
   `DATABASE_URL`, the **same** `JWT_SECRET`.
4. Smoke-test through its Service or a temporary hostname: login, device pairing (`/auth/device/start`
   → `/claim` → `/poll`), a full `ScribbleKeep` sync round trip, `/api/sync/stream` staying open
   past the 240s keep-alive, `/api/devices`, and `/api/user/data` **on a throwaway account only** —
   that last one verifies the profile-scoped deletion for real.
5. Expect cold sync-status watermarks on the new Valkey: the first `/api/sync/status` per user
   falls through to the database. Designed fallback, not a bug.
6. Leave it running with no production traffic for a day.

### Phase 3 — Flip traffic (the reversible one)

1. In the **`teddyfyi` repo**, change `site-ingress` so `api.scribbleroute.com` routes to
   `scribbleroute-api-svc`. Keep the annotation naming both certificates.
2. Watch tablets: sync round trips, SSE reconnects, `/auth/refresh` success rate. Existing sessions
   must keep working — if they do not, `JWT_SECRET` is not identical, and the rollback is the
   ingress change.
3. Once healthy, switch `api-rust-dep` from `APP_PROFILE=both` to `teddy_fyi`. This is the first
   structural narrowing and it is a one-line, instantly reversible env change: from here the old
   deployment *cannot* serve ScribbleRoute at all.
4. Point the reaper CronJob at the ScribbleRoute profile.
5. Rollback at any point: revert the ingress, set `both` back. One database throughout, so nothing
   has diverged.

Sit here as long as you like. Both products now have independent compute, independent Redis and
independent config, on one codebase. **This is the natural place to stabilise the repo before
duplicating it** — every cleanup done here is a cleanup you do once instead of twice.

### Phase 4 — Fork (now a pure refactor)

1. Create `scribbleroute/backend` from a **clone**, not a GitHub fork — the fork relationship
   would have to be broken anyway, and a clone avoids inheriting issue/PR cross-links. Keep full
   history.
2. **Neuter its deploy workflow in the very first commit** (`push.branches: []` or delete it).
   This is what prevents §1.3 while you are still editing.
3. Delete what the `scribbleroute` profile excludes — the profile *is* the list: `src/routes/sync/todo*`,
   `src/routes/sync/grocery*`, `src/routes/lists*`, `src/routes/ai*`, the `Todo`/`Grocery`/`All`
   scope arms, the teddy.fyi half of `delete_user_data`, the list-invite sweep. Then delete the
   profile machinery itself; with one product it has one value.
4. The Phase 1 route-set tests become the fork's contents tests. If one fails, something was
   deleted that the running ScribbleRoute deployment still serves.
5. **Keep the migration files byte-identical to this repo's.** Do not collapse them here. The
   fork's binary is about to be pointed at the *existing* database, whose `_sqlx_migrations` table
   holds this repo's history; a collapsed `0001_init.sql` means `VersionMissing` and a pod that
   never serves (§1.3). Identical files mean matching checksums, nothing to apply on either side,
   and neither binary able to surprise the other. The collapse belongs in Phase 5, where the fork
   creates its own database and it is exactly right.
6. The legacy-token shim (`context/2026-09-05_identity_model.md` §6) ships here, well before the
   freeze that needs it — it is code, not schema, so it is safe in this window.
7. Retarget the image to `gcr.io/…/scribbleroute-api`. The manifests keep the names they have been
   running under since Phase 2.
8. `cargo sqlx prepare -- --tests`; `make test` green.
9. **Hand over ownership of the ScribbleRoute manifests, in this order**, or the two repos will
   fight over the image field on every deploy:
   * merge a commit in *this* repo deleting the `scribbleroute-*` manifests from `k8s/`. The
     resources stay running in the cluster, briefly unmanaged. No downtime.
   * then enable the fork's `deploy.yml` and let its first deploy take ownership with its own
     image.
10. **No migrations in either repo until Phase 5 completes.** From the moment the fork can deploy
    until its database is its own, two migration directories can meet one database and either can
    crashloop the other (§1.3) — a commit in the fork taking down teddy.fyi's next rollout is the
    shared-image-tag hazard in a second costume. A genuinely urgent migration has to land in both
    repos byte-identically, same filename and content, before either deploys. Keep this window
    short: it is the one stretch of the plan with a coupling that nothing enforces. The revised
    ordering already shortens it — under the fork-first version it spanned two whole phases.

### Phase 5 — Cut the data tier (the one freeze)

Everything here happens in the forked repo.

1. Collapse the fork's migrations to a single `0001_init.sql` in the **target identity shape** —
   surrogate `users.id`, `provider`/`subject`, real foreign keys and NOT NULL tenancy columns.
   This is the first moment it is safe (Phase 4 step 5) and the last moment it is free.
2. `CREATE DATABASE scribbleroute;` in the same Neon project. Run that migration against it.
   Verify a boot against it in a scratch pod — the same `VersionMissing` check that constrains
   Phase 4 is what proves this one correct.
3. **Freeze ScribbleRoute writes.** Scale `scribbleroute-api-dep` to zero, or 503 that host at the
   ingress. Small user base, so a short real outage is cheaper than any dual-write scheme. Announce
   it if testers are active.
4. Copy, parents before children. **The copy is also the identity re-key** — see
   `context/2026-09-05_identity_model.md` §8 — which makes it a program, not a `pg_dump`:
   * `users` and `sessions` — the subset selection needs the Rust identity join (mirroring
     `find_stale_users`: read `devices.user_id`/`configs.user_id`/`drawings.user_id`, hash every
     `users.id`, keep the matches). Do *not* try to express it in SQL, and do not copy the whole
     `users` table as a shortcut — that moves grocery-only accounts into the product database,
     the mirror image of the problem being solved. For each row kept: mint a `gen_random_uuid()`,
     write it as the new `users.id` with `provider = 'google'` and `subject` set to the old id,
     and record the mapping.
   * `configs`, `drawings`, `devices`, `device_authorizations`, `device_claim_failures` —
     rewritten through that mapping. The three UUID-keyed tables keep their column type and change
     only their values; the two TEXT-keyed ones narrow to UUID.
   * Rows the mapping cannot reach are the orphans counted in Phase 0. Decide before the freeze.
5. Compare row counts against the Phase 0 figures. Spot-check one account end to end: its device
   list, its config keys, one drawing blob — and that its rows all carry the *same* new `users.id`,
   which is the check that catches a mapping applied inconsistently across tables.
6. Swap `DATABASE_URL` to the new database. Restart. Unfreeze.
7. Rollback: swap `DATABASE_URL` back and restart. The old database took no ScribbleRoute writes
   during the freeze, so it is still authoritative. This reverses the data move and the re-key
   together — rollback granularity is per-window, not per-change, which is why bundling the two
   costs no reversibility.

### Phase 6 — Decommission here

Only after Phase 5 has been healthy for a week, and only from a fresh backup:

1. Delete the ScribbleRoute code: the three `Scribble*` scopes and their future in `sync_handler`,
   `sync/config.rs`, `sync/drawing.rs`, `sync/device.rs`, `src/routes/devices*`,
   `src/dao/config_dao.rs`, `src/dao/drawing_dao.rs`, `src/models/config.rs`,
   `src/models/drawing.rs`, the device-pairing endpoints, `src/jobs/reap_stale_users.rs` and
   `src/jobs/reap_device_authorizations.rs`.
2. Delete the profile machinery. One product, one value, no flag.
3. Narrow `delete_user_data`, `CORS_ALLOWED_ORIGINS` and `APP_VERIFICATION_URIS` to teddy.fyi;
   drop `SCRIBBLEROUTE_API_CLIENT_ID` from the manifest and the audience list.
4. Remove `api-scribbleroute-cert` from `k8s/api-rust.yaml` **and** from the `site-ingress`
   annotation in `teddyfyi`, in that order.
5. Migration dropping `configs`, `drawings`, `devices`, `device_authorizations`,
   `device_claim_failures`, and the `sync_state` enum if nothing else uses it. **Split this across
   two deploys.** `maxSurge: 1` / `maxUnavailable: 0` means the new pod runs its migrations while
   the old pod is still serving the old code, so dropping the tables in the same deploy that
   removes the code leaves the old pod querying tables that are gone. Ship the code removal first,
   the drop second.
6. **Delete the ScribbleRoute-only `users` and `sessions` rows** using the same Rust join from
   Phase 5, inverted. This is the step that actually gets external testers' email addresses out of
   the personal database, and the one most likely to be forgotten because dropping the tables
   *feels* like completion.
7. `cargo sqlx prepare`, tests, deploy.

### Phase 7 — Optional, later

Move the ScribbleRoute database to its own Neon project (option D): `pg_dump`/`pg_restore` and one
secret, no code change. Rotate `JWT_SECRET` on the ScribbleRoute side at the same time, accepting
one forced re-login.

---

## 6. Risk register

| Risk | Phase | Mitigation |
|---|---|---|
| Profile misconfigured — a deployment serves the wrong product | 1–3 | Fail-closed startup on unset/unknown value; `both` logs a warning every boot and is deleted in Phase 6 |
| A ScribbleRoute client relies on the default `All` scope | 1 | Ships under `both` first, and the rejection is logged before it is enforced — production answers this, not a guess |
| Grocery commit restarts the ScribbleRoute pod | 1–4 | Accepted while one repo deploys both: SSE streams cut and clients reconnect. Ends at the fork |
| Isolation is a config value, not a code property | 1–4 | Accepted and time-boxed; the fork is what makes it structural |
| Fork's CI overwrites the shared image tag / fights over manifests | 4 | Disable `deploy.yml` in the fork's first commit; hand over ownership in the documented order (Phase 4 step 9) |
| Two repos, one database: a migration in either crashloops the other | 4→5 | No migrations in either repo in that window; keep it short (Phase 4 step 10). Found by `pre_split_changes` item 1 |
| Forced re-login on every tablet at cutover | 3 | Identical `JWT_SECRET`; `sessions` copied in Phase 5 |
| A client derives `user_id` from the Google token rather than our JWT | 5 | Would break permanently at the re-key and needs a client release first — answered in Phase 0 |
| Cannot select ScribbleRoute `users` rows in SQL | 5, 6 | One-shot Rust subcommand using `parse_or_hash_uuid`, mirroring `find_stale_users` |
| Identity re-key and data move fail together | 5 | Accepted: one window, one rollback (`DATABASE_URL`), old database untouched |
| New pod cannot reach Redis | 2 | Expected — `cache.yaml`'s NetworkPolicy selects `app: api-rust`; the new deployment gets its own Valkey and its own policy |
| External users' rows left behind in the personal DB | 6 | Explicit step 6; dropping tables is not sufficient |
| Two repos drift on the sync protocol | after 4 | Accepted. The wire contract is frozen by the shipped clients; if drift becomes painful, extract a shared crate then, not now |

## 7. What this does not solve

* **Source-level separation, until Phase 4.** Through Phases 1–3 both products still ship on one
  commit and one release cadence, and a grocery-side change still rolls the ScribbleRoute pod.
  That is the deliberate trade: operational isolation first, source isolation once the operational
  shape is proven.
* **Client-side configuration.** The tablets already talk to `api.scribbleroute.com`, so no client
  release is required for the cutover. A client with the teddy.fyi host hard-coded anywhere would
  be, and this plan assumes none does — verify before Phase 3.
* **Anything about the grocery app's own quality.** It keeps every default it has today, and keeps
  raw Google subjects as user ids (`context/2026-09-05_identity_model.md` §10).
