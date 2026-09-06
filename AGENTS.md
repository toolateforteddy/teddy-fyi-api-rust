# AI Context: Sync Backend Engine & API Contract

**Start with [`CLAUDE.md`](CLAUDE.md)** — it is the entry point for working in this
repo: the module map, how to build, test and validate a change here, and the five hard
constraints. This file carries two things that are not there: the sync engine's design
expectations, and deployment (`k8s/`) plus the index of planned work that already has a
written spec. `README.md` carries the endpoint-by-endpoint API contract.

## Project Overview
This Rust service (Axum on Tokio) acts as the centralized Sync Gatekeeper and source of truth for a multi-tenant, local-first Android and iOS ecosystem. It must manage low-concurrency, collaborative data streams (e.g., household shared grocery lists and private user to-do lists).

## Database Architecture & Scoping
The relational backend schema must support data isolation and multi-device filtering via a shared layout:
- `lists` / `zones` table: Tracks list contexts (e.g., Type: `GROCERY` or `TODO`).
- `list_members` table: Many-to-many relationship mapping `user_id` to `list_id`. (e.g., Husband and Wife share Grocery List ID; Husband has exclusive access to Personal Todo List ID).
- `todo_tasks` and `grocery_items` tables: Completely separate tables mapping back to a `list_id`.

## The Sync Endpoint Contract (`POST /api/sync`)
The backend exposes a single, atomic endpoint to reconcile state. 

### Inbound Payload from Client:
- `last_synced_at`: Timestamp (Unix millis) or Sequence Number.
- `client_id`: Unique identifier to prevent echoing changes back to the sender.
- Separate transaction delta arrays: `todo_changes` and `grocery_changes` containing `id (UUID)`, `type (INSERT/UPDATE/DELETE)`, `version`, and the updated `data` object.

### Server Transaction Logic (Per Incoming Change):
1. **Permission Check:** Verify the requesting `user_id` belongs to the target `list_id`.
2. **Conflict Detection (MVCC):** - For `UPDATE`, compare incoming `client.version` against current `server.version`.
   - If versions match: Apply payload, increment `server.version += 1`, mark as success.
   - If conflict occurs (`client.version < server.version`): Fall back to field-level or implicit **Last Write Wins (LWW)**, overwrite the state, but pass back the newly bumped server version to force the client to align its local state counter.
3. **Delta Extraction:** Query tables for any modifications where `updated_at > client.last_synced_at` AND `updated_by != client.user_id`.

### Outbound Response Payload:
- `upload_status`: Confirmation of successfully processed client adjustments and their new server version IDs.
- `remote_changes`: Arrays of updates/deletes that occurred on the server since the client's `last_synced_at`.
- `server_timestamp`: The current atomic server time to act as the client's next `last_synced_at` anchor.

## Expectations for anything written against the sync engine
1. Prioritize strict type safety, transaction isolation, and explicit error handling for database writes.
2. Ensure the sync engine avoids the "echo" problem by accurately utilizing the `client_id` filter.
3. Keep the payload formats perfectly mirrored to the Android client schema requirements.
4. Follow the module layout rule — no `mod.rs`, entry files declarative — which is
   constraint 2 in [`CLAUDE.md`](CLAUDE.md) and is checked by
   `scripts/check_constraints.sh`.

## Deployment and the `k8s/` manifests

The GKE manifests for this service live in `k8s/` and are applied by
`.github/workflows/deploy.yml` on every merge to `main`. They used to live in the
`teddyfyi` repo (the nginx/hosting repo) and be applied by hand from a laptop; they moved
here so that a manifest change and the code that needs it ship in one commit.

**The image is pinned to the commit SHA, and the manifests do not name a tag that moves.**
Both `api-rust.yaml` and `user-reaper.yaml` say
`teddy-fyi-api-rust:IMAGE_TAG_PLACEHOLDER`; the deploy workflow rewrites that on the
`image:` lines to `github.sha` before it applies the directory. Three things follow:

* **Rollback is `kubectl set image` to an older SHA**, or a revert, rather than a rebuild.
  `kubectl describe deploy api-rust-dep` now says which commit is running.
* **`kubectl apply -f k8s/` by hand does not work** — the pod fails to pull the
  placeholder. That is deliberate; the alternative failure is a hand-apply silently
  resolving `:latest` to whoever pushed last, which after the fork is not necessarily us.
  Substitute a SHA yourself if you really mean to apply from a laptop.
* **A deploy no longer restarts the pods when the spec is unchanged.** `apply` starts the
  rollout because the image changed, so the old unconditional `rollout restart` is gone —
  which means re-running the workflow on the same commit is a no-op. To pick up a secret
  rotated in Secret Manager with no code change, run
  `kubectl rollout restart deployment/api-rust-dep` deliberately.

`scripts/check_constraints.sh` fails if a manifest goes back to a moving tag.

| File | What it is |
|---|---|
| `k8s/api-rust.yaml` | `api-rust-svc` (NodePort), `api-rust-ksa`, the `SecretProviderClass`, `api-rust-dep`, the `BackendConfig`, and the `api-rust-cert` / `api-scribbleroute-cert` ManagedCertificates. |
| `k8s/cache.yaml` | The Valkey `cache-dep` / `cache-svc` this service uses for Redis, plus a NetworkPolicy admitting only `app: api-rust`. |
| `k8s/user-reaper.yaml` | The daily retention CronJob. Reuses this image with the `reap-stale-users` subcommand and the same KSA and secret mount. |
| `k8s/maintenance.yaml` | The maintenance responder for `api.scribbleroute.com`: an nginx answering 503 with an explanation, for the Phase 4 write freeze. **Applying it starts it; it takes no traffic until `site-ingress` points the hostname at it.** See `context/2026-09-05_planned_maintenance.md`. |

**What is *not* here:** `site-ingress` stays in `teddyfyi`, because it is the shared front
door for the nginx site as well as this API. It references `api-rust-svc` and names both
of our certificates in its `networking.gke.io/managed-certificates` annotation, so
renaming a Service or a ManagedCertificate here is a cross-repo change. The hostnames it
routes to us are `api-rust.teddy.fyi` and `api.scribbleroute.com`.

**Secrets.** Values live in GCP Secret Manager and must never appear in `k8s/`. What lives
here is the *wiring*, and adding one secret-backed variable is three edits to
`api-rust.yaml`: a `parameters.secrets` entry, a `secretObjects.data` mapping, and a
container `env.valueFrom.secretKeyRef`. Creating the secret itself is still an
out-of-band `gcloud secrets create`.

**Most tunables are unset in prod.** The code reads roughly forty environment variables;
the manifests set eleven. Everything else — the `LIST_*` invite limits, `SSE_MAX_STREAMS_*`,
`GEMINI_MAX_CALLS_*`, the rate-limit and guardrail knobs, `CORS_ALLOWED_ORIGINS`,
`COOKIE_DOMAIN` — runs on the compiled-in default. That is a deliberate starting point, not
an oversight, but it does mean the default in the Rust source *is* the production value.
Change one and you are changing prod.

**Why this matters for the planned fork.** The split is now planned in full in
`context/2026-09-05_scribbleroute_backend_split.md`; read it before touching any of this. `scribbleroute/backend` is expected to fork from
this repo and take `api.scribbleroute.com`, leaving this one defaulting to
`api-rust.teddy.fyi`. The fork inherits `k8s/` as its starting point. Two things to know
when that happens: every resource name in `api-rust.yaml` is currently unqualified
(`api-rust-dep`, `api-rust-svc`, `api-rust-ksa`), so the fork must rename or the two repos
will apply over each other in the same cluster; and `cache.yaml` is the one file that is
plausibly *shared* infrastructure rather than per-service, so it should end up singly
owned rather than duplicated.

## Planned work with a written spec

Before designing anything in these areas, read the note — the decisions are already made and the
endpoint shapes are already written down.

- **Auth on devices with no Google Play Services** (Fire tablets, and any future shared-device
  install) — `context/2026-09-04_device_pairing_auth.md`. **Built**, in `src/auth/device.rs`:
  `/auth/device/start`, `/auth/device/claim` and `/auth/device/poll`, the `device_authorizations`
  migration, and `auth::handlers::issue_session` extracted out of `login_handler`. Read the note
  before changing any of it — the response codes are load-bearing, several of them deliberately
  indistinguishable from each other. Cross-repo: the Android client and the
  `scribbleroute.com/link` page are the other two halves and are not built here.

  Two products now pair through these endpoints, and they redeem codes on two different
  websites, so `/auth/device/start` reads the `app` the client sends and answers with that
  app's page: `SCRIBBLE_KEEP`/`SCRIBBLE_BOX` → `scribbleroute.com/link`, `TEDDY_FYI`/
  `TEDDY_FYI_GROCERY` → `teddy.fyi/link` (`APP_VERIFICATION_URIS` in `src/auth/device.rs`).
  A new client that pairs adds its wire name there, or it will send its parents to the
  default page — which is somebody else's site.

- **Splitting the ScribbleRoute backend out** — `context/2026-09-05_scribbleroute_backend_split.md`.
  **Planned, not started.** Records what is genuinely shared between the two products (auth and
  the `users` table, not just Postgres), the ordering that keeps each risky step independently
  reversible, and the two ways a naive fork takes down production on its first green build — it
  inherits this repo's image tag and its unqualified k8s resource names. **Revised:** the fork is
  no longer first — an `APP_PROFILE` flag splits the two products inside this repo and carries the
  second deployment and the traffic cutover, so the fork becomes a refactor against a system
  already running in the target shape. Read it before creating `scribbleroute/backend`, before removing anything ScribbleRoute-shaped from this repo, and
  before changing `site-ingress` in `teddyfyi`.

- **The identity model we are moving to** — `context/2026-09-05_identity_model.md`.
  **Designed, not built.** `users.id` becomes an opaque surrogate UUID and the Google subject
  becomes an attribute (`provider`, `subject`), which ends the two-identity split below and makes
  room for a second sign-in provider. Accounts are deliberately *not* linked across providers. The
  re-key rides along in Phase 5 of the split, because that freeze is the only window in which it
  is cheap, and the fork has to precede it. Read it before adding a sign-in provider, before keying a new table by a user, and
  before writing the Phase 4 copy program.

- **Telling users about a planned outage** — `context/2026-09-05_planned_maintenance.md`.
  **Built, never used.** The runbook for Phase 4's write freeze: `k8s/maintenance.yaml` here, the
  `site-ingress` backend swap in `teddyfyi`, and the status document on `scribbleroute.com` that
  the ScribbleKeep apps read to tell a planned outage apart from a tablet with no Wi-Fi. Read it
  before running any window, and note the one detail that is easy to get wrong: the maintenance
  nginx's health check must answer 200, or the GKE ingress marks it unhealthy and serves Google's
  502 page instead of the explanation.

- **Changes to make before or during the split** — `context/2026-09-05_pre_split_changes.md`.
  **A survey, not a plan.** Forty-five scored items that are cheap while there is one repo and
  expensive once there are two, or that need Phase 4's write freeze. It is written to be worked
  in parallel — each item stands alone and names its files — and its "Working these in parallel"
  section states which items must land together, which are decisions rather than patches, and
  which must *not* be started before Phase 4. Read it before picking up work in this window, and
  amend it in place as items land rather than deleting them.

  One item is a correction rather than a suggestion and has already changed the split plan:
  `db::init_postgres` runs `sqlx::migrate!` on every boot, which the split note said nothing
  does. Auto-migration **stays** — it is right for one-database-per-service, which is where
  Phase 4 lands. What changed is the sequencing: the fork keeps the inherited migration files
  through Phases 2–3 and collapses them to `0001_init.sql` only in Phase 4, and Phases 2–3
  carry a no-migrations-in-either-repo constraint. Read split plan §1.2 before touching
  `migrations/` or `src/db.rs`.

- **The two user identities** — `context/2026-09-05_user_identity_derivation.md`. The same
  signed-in person is named two ways: the raw auth subject keys `users`, `sessions` and every
  todo/grocery table, while `parse_or_hash_uuid(sub)` keys `configs`, `drawings` and `devices`.
  **Documented, not fixed** — re-keying orphans every existing row, so the change is deferred and
  the note costs the migration instead. Read it before touching `parse_or_hash_uuid`, before
  adding a table keyed by a user, and before writing any endpoint that takes a user identifier
  from a request body. `src/routes/sync/tests/identity.rs` pins the current behaviour and is
  supposed to fail if the derivation moves.
