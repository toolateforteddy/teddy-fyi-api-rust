# Splitting the ScribbleRoute backend out of `teddy-fyi-api-rust`

*Written 2026-09-05, before any of it is built. Status: **plan, not yet started**.*

This service currently answers for two products from one process, one image, one
database and one Redis:

* **ScribbleRoute** — ScribbleKeep / ScribbleBox on Fire tablets, launched to internal
  testing, with real external users, a published privacy policy at
  <https://scribbleroute.com/privacy>, and its own hostname `api.scribbleroute.com`.
* **teddy.fyi** — the personal grocery and todo apps, one household, no external users,
  no published policy, hostname `api-rust.teddy.fyi`.

The goal is to separate them. The identity re-key that rides along in Phase 4 has its own note:
`context/2026-09-05_identity_model.md`.

This note records *what is actually entangled* (read out
of the code, not assumed), the options considered, the option chosen, and the ordered
steps with their rollbacks. Read the entanglement section before deciding anything —
several of the couplings are not where you would guess, and two of them will break
production on day one if the fork is created naively.

---

## 1. What is actually shared today

### 1.1 The seam that is already clean

`SyncScope` (`src/routes/sync/types.rs`) partitions the sync endpoint by product, and the
handler dispatches on it into three independent futures with independent transactions:

| Scope | Product | Tables |
|---|---|---|
| `Todo`, `Grocery` | teddy.fyi | `todo_lists`, `todo_items`, `grocery_lists`, `grocery_list_members`, `grocery_items`, `grocery_item_store_info`, `stores`, `categories`, `list_invites`, `list_join_failures` |
| `ScribbleBox`, `ScribbleKeep`, `ScribbleKeepCloud` | ScribbleRoute | `configs`, `drawings`, `devices` |

Nothing in the ScribbleRoute branch of `sync_handler` reads a grocery or todo table, and
nothing in the grocery/todo branch reads `configs` or `drawings`. The AI routes
(`/api/categorize`, `/api/assign-icon`) and the list-invite routes (`/api/lists/*`) are
teddy.fyi-only. `/api/devices/*` is ScribbleRoute-only. So the *code* cut is close to
mechanical: delete whole modules, delete whole route lines.

That is the easy 20%.

### 1.2 The couplings that are not clean

**Auth is one tier, not two.** Both products validate against one `JWT_SECRET`, one
audience set (`SCRIBBLEROUTE_API_CLIENT_ID` sits in the same `SINGLE_ID_ENV_VARS` list as
`GOOGLE_CLIENT_ID` and `GOOGLE_CLIENT_ID_GROCERY_WEB`, see `src/auth/client_ids.rs`), and
one `sessions` table keyed `(user_id, client_uuid)`. A token minted for a tablet is
structurally acceptable to the grocery backend and vice versa; today they are the same
backend, so this has never mattered. It starts mattering the moment there are two.

**`users` is shared and cannot be partitioned in SQL.** `users.id` is the raw Google auth
subject (TEXT). `configs.user_id`, `drawings.user_id` and `devices.user_id` are UUIDs
derived by `parse_or_hash_uuid(sub)` — a *one-way* hash for non-UUID subjects. This is the
subject of `context/2026-09-05_user_identity_derivation.md`, and its consequence here is
concrete: **you cannot write a SQL query that selects "the `users` rows belonging to
ScribbleRoute accounts."** `find_stale_users` in `src/jobs/reap_stale_users.rs` already
works around this by doing the join in Rust. Any data-tier split has to do the same, or
copy the whole table.

**Deletion is a single function spanning both universes.** `delete_user_data`
(`src/routes/user/deletion.rs`) erases todo, grocery, configs, drawings, devices,
device authorizations, sessions and the user row in one transaction. `DELETE
/api/user/data` and the retention reaper both go through it. A ScribbleRoute user
exercising their delete right today deletes grocery rows in the same transaction — which
is correct while it is one account in one product, and is exactly the entanglement that
prompted this split.

**The reaper's eligibility rule is a workaround for the shared table.** From its own
module doc: only accounts with a `devices` row are in scope, because `users` is shared
with the grocery side and no published policy covers those accounts. That rule exists
solely because of the sharing, and should disappear with it.

**Redis keys are shared but namespaced by scope.** Keys are `user:{sub}:last_update:{scope}`
(`sync_handler`, `sync/status.rs`, `deletion.rs`) and `ai:gemini:calls:user:{id}:{day}`.
The scope suffix means there is no *collision* risk between products today. The real
shared-Redis risks are eviction pressure, a stray `FLUSHDB`, and the SSE pub/sub fanout
channels — not key names.

**`COOKIE_DOMAIN` defaults to `.teddy.fyi` and is not set in the manifests**, so that is
the production value for *both* hostnames. `session_cookie` therefore emits
`Domain=.teddy.fyi` on responses served from `api.scribbleroute.com` — which a browser on
`scribbleroute.com/link` discards. This is a latent bug that the split is the natural
occasion to fix (`COOKIE_DOMAIN=.scribbleroute.com` on the new deployment).

**`CORS_ALLOWED_ORIGINS` is unset in prod**, so the compiled default in `main.rs`
(`teddy.fyi` + both spellings of `scribbleroute.com`) *is* the production value. Both
sides need to narrow their default after the split.

### 1.3 The two things that will break production on day one

Both come from the fork inheriting `.github/workflows/deploy.yml` and `k8s/` verbatim:

1. **The image tag is shared.** `deploy.yml` pushes
   `gcr.io/melodic-sunbeam-164916/teddy-fyi-api-rust:latest`. A fork that merges to `main`
   overwrites the *running* teddy.fyi image, and the next `rollout restart` anywhere pulls
   the fork's binary into `api-rust-dep`.
2. **Every k8s resource name is unqualified** (`api-rust-dep`, `api-rust-svc`,
   `api-rust-ksa`, `api-rust-gcp-secrets`, `api-rust-be-config`, plus both
   ManagedCertificates), and `deploy.yml` runs `kubectl apply -f k8s/` followed by
   `kubectl rollout restart deployment/api-rust-dep`. A fork's first green build applies
   over the original's manifests and restarts the original's deployment.

`AGENTS.md` already flags the naming collision. The image tag is the one that is easy to
miss, and it is worse: it is silent.

Two more cross-repo facts:

* `site-ingress` lives in the **`teddyfyi` repo**, not here. It references `api-rust-svc`
  by name and names both ManagedCertificates in its
  `networking.gke.io/managed-certificates` annotation. The traffic cutover is a change in
  *that* repo.
* `cache.yaml`'s NetworkPolicy admits only `podSelector: app: api-rust`. A new pod with a
  different label cannot reach `cache-svc` at all — which is convenient, since it means a
  misconfigured new deployment fails closed rather than quietly sharing Redis.
* ~~Prod migrations are applied **by hand**. Nothing in `deploy.yml` or the binary runs
  `sqlx migrate run` against production; CI runs it against its own throwaway Postgres.
  So the "two services racing migrations on boot" hazard does not exist here. The hazard
  is a human applying the wrong repo's migrations to the wrong database.~~

  **Wrong, corrected 2026-09-05.** `deploy.yml` does not, but the **binary does**:
  `db::init_postgres` (`src/db.rs:202`) runs `sqlx::migrate!("./migrations").run(&pool)`
  on every start, and `init_app_state` calls it. So every rollout restart applies that
  image's migration directory to whatever `DATABASE_URL` points at.

  The racing hazard is therefore real, and **Phase 2 is exactly the configuration that
  triggers it**: two deployments, one database, each auto-applying its own directory —
  and Phase 1 step 3 deliberately makes those directories incompatible, so the fork's
  first boot tries to apply `0001_init.sql` to a database whose `_sqlx_migrations` table
  already has eighteen rows. Resolve this before Phase 2; the options are in
  `context/2026-09-05_pre_split_changes.md` item 1.

---

## 2. Options considered

Isolation has three independent axes — compute, database, Redis — and they can be cut at
different times. The options below are the combinations worth naming.

| | Compute | Postgres | Redis | Repo | Migration effort |
|---|---|---|---|---|---|
| **A. Two deployments, one repo** | separate | shared | shared or separate | one | none |
| **B. Fork, shared data tier** | separate | shared | shared | two | none |
| **C. Fork, separate DB + Redis** | separate | separate DB, same Neon project | separate pod | two | one copy + freeze |
| **D. Full isolation** | separate | separate Neon project | separate pod | two | one copy + freeze + new account wiring |
| **E. Cargo workspace, one deployment** | shared | shared | shared | one | refactor |

**A — two Deployments from one image, selected by an env var.** The cheapest compute
isolation there is: ScribbleRoute traffic can no longer be taken down by a grocery-side
panic, memory leak, or a runaway AI call, and it needs no fork, no new CI, and no data
work. It does nothing for the data tier and nothing for the deletion problem, and it
leaves external-product code shipping on the same commit as personal-project code. Worth
knowing about mainly as a *fallback*: if the fork stalls halfway, this is a safe place to
stand.

**B — fork, keep sharing Postgres and Redis.** This is the original instinct. It buys the
compute isolation, the independent release cadence, and a repo whose contents you would
not mind an outside contributor reading. It leaves the deletion footgun, the shared
`users` table, and a shared Redis exactly where they are — which is the specific thing the
itch is about. As a *destination* it is not enough; as a *waypoint* it is excellent,
because it decouples the risky ingress cutover from the risky data copy.

**C — fork, dedicated database in the same Neon project, dedicated Redis pod.** Deletion
becomes structurally incapable of touching grocery rows. Retention becomes a policy over a
database that contains only accounts the policy covers. A bad migration hits one product.
The cost is one data copy under a freeze, plus the accepted consequence that a person who
uses both products becomes two independent accounts (see §4).

**D — separate Neon project.** Everything in C, plus separate credentials, separate
billing, and a boundary that survives ScribbleRoute changing hands. The extra work over C
is small (a project, a connection string, a secret) and the extra ongoing cost is the free
tier or thereabouts. The reason not to start here is that C's copy step is the same work,
and C keeps a shared psql session for the copy itself.

**E — Cargo workspace.** Solves code drift, solves nothing else. It is the wrong tool for
this problem: the itch is about blast radius and data, and a workspace changes neither.

## 3. Chosen path

**Fork to `scribbleroute/backend`, cut traffic over while still sharing the data tier
(option B), then split Postgres and Redis in a second, separately-rollbackable step
(option C), then decommission on the teddy.fyi side.** Leave the door open to D later —
C's schema is already the whole database, so moving it to its own Neon project is a
`pg_dump`/`pg_restore` and a secret, with no code change.

**Why traffic first, data second** — this is the one place this plan deliberately differs
from the obvious ordering. Doing the data copy *before* the ingress flip means the new
backend is serving a snapshot while the old backend is still taking live writes for the
same users: a split brain whose reconciliation is manual. Doing the flip first means each
risky step has one variable and its own rollback:

* the ingress flip is one annotation change, reversible in a minute, with both backends
  reading the same data, so a rollback loses nothing;
* the data cut is a freeze → copy → `DATABASE_URL` swap → restart, reversible by swapping
  the variable back, because the old database kept taking no writes during the window.

The counterargument is that between the flip and the cut there is a window where a
ScribbleRoute request can still reach grocery data through a bug in the new backend. That
window is real, it is measured in days, and it is a strictly smaller risk than a
split-brain reconciliation.

---

## 4. Decisions this plan bakes in

Write these down now; they are the ones that are expensive to reverse.

1. **A person who uses both products becomes two accounts.** Same Google identity, two
   `users` rows in two databases, two `sessions` rows, no shared state. This is the point
   of the split, and it is fine — the overlap is the author's own household.
2. **`JWT_SECRET` stays identical across the cutover.** Both backends must accept the
   tokens already in the field, or every tester is logged out at the flip. Rotating it is
   a separate, later, deliberate change on the ScribbleRoute side only.
3. **`sessions` and `device_authorizations` rows get copied**, for the same reason:
   a `/auth/refresh` that 401s means a forced re-login on every tablet, and an in-flight
   pairing that fails means a parent retries a code that no longer exists.
4. **The retention reaper moves to the ScribbleRoute repo and is deleted here.** It
   implements a ScribbleRoute policy. Its `users`-is-shared eligibility rule (only
   accounts with a `devices` row) can then be dropped, and the sweep can consider every
   account in its own database.
5. **`cache.yaml` is not duplicated.** The existing Valkey stays teddy.fyi's; ScribbleRoute
   gets a new one under its own name. Its NetworkPolicy must select the new pod label.
6. **The teddy.fyi database keeps no ScribbleRoute user rows.** Dropping the tables is not
   enough — external testers' `users` and `sessions` rows have to go too, and selecting
   them requires the Rust-side identity join (§1.2).
7. **The identity re-key rides along in Phase 4.** `users.id` becomes an opaque surrogate
   UUID and the Google subject becomes an attribute (`provider`, `subject`) — the design is
   `context/2026-09-05_identity_model.md`. Added after this plan was first written: the
   six-step dual-write migration that fix would otherwise need exists *only* because there
   is no maintenance window, and Phase 4 is that window. Accounts are **not** linked across
   providers; a second provider means a second account, deliberately.

---

## 5. Steps

Each phase ends in a state that is safe to sit in indefinitely. Do not start the next one
until the current one has been observed healthy for at least a day of real tablet traffic.

### Phase 0 — Prepare, change nothing (no user-visible effect)

1. Create `scribbleroute/backend` from a **clone**, not a GitHub fork — the fork
   relationship would have to be broken anyway, and a clone with a fresh `git init`-style
   push avoids inheriting issue/PR cross-links. Keep full history: the sync engine's
   commit archaeology is worth more than a clean start.
2. **Before the first push to `main` in the new repo, neuter its deploy workflow.** Set
   `push.branches: []` or delete `deploy.yml` outright. This is what prevents §1.3 from
   happening while you are still editing. Do this in the very first commit.
3. Take a Neon backup / branch of the current database. It is the rollback for everything
   below that touches data.
4. Write down the current row counts for `configs`, `drawings`, `devices`,
   `device_authorizations`, `device_claim_failures`, `sessions`, `users`. They are the
   check figures for Phase 3.
5. Two questions the identity re-key in Phase 4 depends on, both cheap and both better
   answered now than under a freeze (`context/2026-09-05_identity_model.md` §12):
   * the **orphan audit** — `configs`/`drawings`/`devices` rows whose `user_id` is the hash
     of no surviving `users.id`. Some are expected: those two tables predate `users` by one
     migration. A large count makes the re-key a different conversation.
   * whether the Android and iOS clients read `sub` from **our** JWT or take Google's `sub`
     from the Google ID token. The second answer means a client release before Phase 4.

### Phase 1 — Carve the new repo (still not deployed)

In `scribbleroute/backend`:

1. Delete the teddy.fyi surface:
   * `src/routes/sync/todo.rs`, `src/routes/sync/todo/`, `src/routes/sync/grocery.rs`,
     `src/routes/sync/grocery/`, `src/routes/lists.rs`, `src/routes/lists/`,
     `src/routes/ai.rs`, `src/routes/ai/`, and their tests.
   * The `Todo` and `Grocery` arms of `SyncScope`, the two corresponding futures in
     `sync_handler`, and their arms in `sync/status.rs`.
   * The grocery/todo half of `delete_user_data` and `DeletedCounts`.
   * The `/categorize`, `/assign-icon` and `/lists/*` routes in `main.rs`, plus
     `GEMINI_API_KEY` (note: `init_app_state` `expect`s it — the removal must reach
     `AppState`, `state.rs` and the manifest together or the pod will not boot).
2. Build the migrations against the **target** identity schema, not today's — surrogate
   `users.id`, `provider`/`subject`, real foreign keys. It is a new database, so there is no
   add-column-and-backfill dance to write. The legacy-token shim
   (`context/2026-09-05_identity_model.md` §6) ships and is tested here too, well before the
   freeze that needs it.
3. Collapse the migrations to a single `0001_init.sql` capturing exactly the ScribbleRoute
   schema in its target shape. The identity change decides this: the inherited files create
   `users.id` as `TEXT` and would have to be undone by a later migration in the same run, so
   replaying history here buys nothing and costs a contradiction. The legacy `TEXT`-to-UUID
   archaeology in the old files is likewise dead weight against a database with no rows yet.
4. Narrow the compiled-in defaults: `CORS_ALLOWED_ORIGINS` to the scribbleroute.com
   spellings, `COOKIE_DOMAIN` to `.scribbleroute.com`, `APP_VERIFICATION_URIS` to the two
   `SCRIBBLE_*` apps.
5. Drop `GOOGLE_CLIENT_ID_GROCERY_WEB` from `SINGLE_ID_ENV_VARS`; keep
   `SCRIBBLEROUTE_API_CLIENT_ID` and the iOS list.
6. Rename every k8s resource (`scribbleroute-api-dep`, `-svc`, `-ksa`,
   `scribbleroute-api-gcp-secrets`, `-be-config`), move `api-scribbleroute-cert` here, and
   add `scribbleroute-cache-dep` / `-svc` with a NetworkPolicy selecting the new API pod
   label. Point `REDIS_URL` at `scribbleroute-cache-svc:6379`.
7. Retarget the image to `gcr.io/…/scribbleroute-api` and the rollout to the new
   deployment name. Re-enable `deploy.yml` only after both are changed.
8. `cargo sqlx prepare -- --tests` to regenerate `.sqlx` against the trimmed schema; `make
   test` green.

**Do not yet remove `api-scribbleroute-cert` from this repo** — until the ingress moves,
this repo is still the one applying it.

### Phase 2 — Deploy alongside, sharing the data tier

1. `kubectl apply` the new manifests. `DATABASE_URL` points at the **existing** database,
   `JWT_SECRET` is the **same secret**, `REDIS_URL` points at the **new** Valkey.
2. Smoke-test it directly through its Service or a temporary hostname: login, device
   pairing (`/auth/device/start` → `/claim` → `/poll`), a full `ScribbleKeep` sync round
   trip, `/api/sync/stream` staying open past the 240s keep-alive, `/api/devices`,
   `/api/user/data` **on a throwaway account only**.
3. Note the expected consequence of the new Redis: sync *status* watermarks start cold, so
   the first `/api/sync/status` per user falls through to the database. That is the
   designed fallback, not a bug.
4. Leave it running with no production traffic for a day.

### Phase 3 — Cut traffic over (the reversible one)

1. In the **`teddyfyi` repo**, change `site-ingress` so `api.scribbleroute.com` routes to
   `scribbleroute-api-svc` instead of `api-rust-svc`. Keep the certificate annotation
   naming both certs.
2. Watch tablets: sync round trips, SSE reconnects, `/auth/refresh` success rate. Existing
   sessions must keep working — if they do not, the `JWT_SECRET` is not identical, and the
   rollback is the ingress change.
3. Rollback: revert the ingress. Both backends are still on the same database, so nothing
   has diverged.

Sit here for a few days. Both products now have independent compute; the itch is half
scratched and nothing has moved in the database yet.

### Phase 4 — Cut the data tier (the one with a freeze)

1. `CREATE DATABASE scribbleroute;` in the same Neon project. Run the new repo's
   migrations against it. Verify the schema matches what the binary expects (`sqlx migrate
   info`, plus a boot against it in a scratch pod).
2. **Freeze ScribbleRoute writes.** Scale `scribbleroute-api-dep` to zero, or return 503
   from the ingress for that host. Small user base, so a short real outage is cheaper than
   any dual-write scheme. Announce it if testers are active.
3. Copy, in this order (parents before children). **The copy is also the identity
   re-key** — see `context/2026-09-05_identity_model.md` §8, and note that this makes the
   copy a program, not a `pg_dump`:
   * `users` and `sessions` — **the subset selection needs the Rust identity join**. Write
     a one-shot subcommand in the new repo (mirroring `find_stale_users`: read
     `devices.user_id`/`configs.user_id`/`drawings.user_id`, hash every `users.id` with
     `parse_or_hash_uuid`, keep the matches). Do *not* try to express it in SQL, and do not
     copy the whole `users` table as a shortcut — that would move grocery-only accounts
     into the product database, which is the mirror image of the problem being solved.
     For each row kept: mint a `gen_random_uuid()`, write it as the new `users.id` with
     `provider = 'google'` and `subject` set to the old id, and record the mapping.
   * `configs`, `drawings`, `devices`, `device_authorizations`, `device_claim_failures` —
     rewritten through that mapping. The three UUID-keyed tables keep their column type and
     change only their values; the two TEXT-keyed ones narrow to UUID.
   * The same hash join finds the `configs`/`drawings`/`devices` rows in the first place,
     because the old derivation cannot be inverted. Rows it cannot map are the derivation
     note's step-2 orphans — count them in Phase 0 (below), decide before the freeze.
4. Compare row counts against the Phase 0 figures. Spot-check one account end to end:
   its device list, its config keys, one drawing blob — and that its rows all carry the
   *same* new `users.id`, which is the check that catches a mapping applied inconsistently
   across tables.
5. Swap `DATABASE_URL` on `scribbleroute-api-dep` to the new database. Restart. Unfreeze.
6. Rollback: swap `DATABASE_URL` back and restart. The old database took no ScribbleRoute
   writes during the freeze, so it is still authoritative and nothing is lost. This reverses
   the data move and the identity re-key together — rollback granularity here is per-window,
   not per-change, which is why bundling the two costs no reversibility.

### Phase 5 — Decommission on this side

Only after Phase 4 has been healthy for a week, and only from a fresh backup:

1. Delete the ScribbleRoute code here: the three `Scribble*` scopes and their future in
   `sync_handler`, `sync/config.rs`, `sync/drawing.rs`, `sync/device.rs`,
   `src/routes/devices*`, `src/dao/config_dao.rs`, `src/dao/drawing_dao.rs`,
   `src/models/config.rs`, `src/models/drawing.rs`, the device-pairing endpoints, and
   `src/jobs/reap_stale_users.rs` + `k8s/user-reaper.yaml`.
2. Narrow `delete_user_data` to the grocery/todo tables and `CORS_ALLOWED_ORIGINS` /
   `APP_VERIFICATION_URIS` to teddy.fyi.
3. Drop `SCRIBBLEROUTE_API_CLIENT_ID` from the manifest and the audience list.
4. Remove `api-scribbleroute-cert` from `k8s/api-rust.yaml` **and** from the `site-ingress`
   annotation in `teddyfyi`, in that order.
5. Migration to drop `configs`, `drawings`, `devices`, `device_authorizations`,
   `device_claim_failures`, and the `sync_state` enum if nothing else uses it.
6. **Delete the ScribbleRoute-only `users` and `sessions` rows** using the same one-shot
   join from Phase 4, inverted. This is the step that actually gets external testers' email
   addresses out of the personal database, and it is the one most likely to be forgotten
   because dropping the tables *feels* like completion.
7. `cargo sqlx prepare`, tests, deploy.

### Phase 6 — Optional, later

Move the ScribbleRoute database to its own Neon project (option D) once it is standing on
its own: `pg_dump` / `pg_restore` and one secret rotation, no code change. Rotate
`JWT_SECRET` on the ScribbleRoute side at the same time, accepting one forced re-login.

---

## 6. Risk register

| Risk | Phase | Mitigation |
|---|---|---|
| Fork's CI overwrites the shared image tag / applies over `api-rust-dep` | 0–1 | Disable `deploy.yml` in the fork's first commit; rename image and every resource before re-enabling |
| Forced re-login on every tablet at cutover | 3 | Identical `JWT_SECRET`; copy `sessions` in Phase 4 |
| Cannot select ScribbleRoute `users` rows in SQL | 4, 5 | One-shot Rust subcommand using `parse_or_hash_uuid`, mirroring `find_stale_users` |
| Split brain if data is copied before traffic moves | — | Ordering: flip first, copy second, freeze during the copy |
| Both deployments auto-migrate the shared database on every restart | 2 | **Open.** The binary runs `sqlx::migrate!` at boot (§1.2 correction), and Phase 1 step 3 gives the fork an incompatible migration directory. Must be resolved before Phase 2 — `context/2026-09-05_pre_split_changes.md` item 1 |
| A ScribbleRoute token is accepted by the teddy.fyi backend, and vice versa | 2–5 | **Open.** `JWT_SECRET` is shared by decision #2 and the token carries no product claim, so the window is the whole of Phases 3–5. Needs the claim minted before Phase 3 — `context/2026-09-05_pre_split_changes.md` item 2 |
| New pod cannot reach Redis | 2 | Expected — `cache.yaml`'s NetworkPolicy selects `app: api-rust`; the new deployment gets its own Valkey and its own policy |
| Pod will not boot after removing Gemini | 1 | `init_app_state` `expect`s `GEMINI_API_KEY`; remove code, `AppState` field and manifest env together |
| External users' rows left behind in the personal DB | 5 | Explicit step 6; dropping tables is not sufficient |
| A client derives `user_id` from the Google token rather than our JWT | 4 | Would break permanently at the re-key and needs a client release first — check both clients before scheduling (identity note §7) |
| Identity re-key and data move fail together | 4 | Accepted: one window, one rollback (`DATABASE_URL`), old database untouched |
| Two repos drift on the sync protocol | after | Accepted. The wire contract is frozen by the shipped clients, and the ScribbleRoute side is now free to evolve its own; if drift becomes painful, extract a shared crate then, not now |

## 7. What this does not solve

* **~~The two user identities.~~** *Superseded — this is now in the plan.* The first
  version of this note deferred the re-key on a "one irreversible change per window"
  instinct. That instinct was wrong here: the rollback is `DATABASE_URL` back to an
  untouched database either way, so bundling costs no reversibility, and Phase 4 is the
  only window in which the fix is cheap. The design is
  `context/2026-09-05_identity_model.md`; the mechanics are in Phase 4 step 3 above. Note
  that the fix is *not* the raw-subject re-keying suggested here — a second identity
  provider rules that out.
* **Client-side configuration.** The tablets already talk to `api.scribbleroute.com`, so
  no client release is required for the cutover. A client that has the teddy.fyi host
  hard-coded anywhere would be, and this plan assumes none does — verify before Phase 3.
* **Anything about the grocery app's own quality.** It keeps every default it has today.
