# Changes to make before or during the split

*Written 2026-09-05, against `07aa0fb`. A survey, not a plan — nothing here is scheduled.*

Companion docs: [`2026-09-05_scribbleroute_backend_split.md`](2026-09-05_scribbleroute_backend_split.md)
(the split itself), [`2026-09-05_identity_model.md`](2026-09-05_identity_model.md) (the
re-key), [`2026-09-05_user_identity_derivation.md`](2026-09-05_user_identity_derivation.md)
(what is true today).

The premise: after the fork, every change to code or schema that both products share costs
two pull requests, two review cycles, two deploys, and a decision about whether the two
copies are allowed to diverge. The data cut also supplies the only write-freeze this service
will ever get. So there is a window — now, and that freeze — in which some changes are cheap and
after which they are not.

Each item is scored 1–10. **10** means "do this weekend regardless of the split". **1** means
"you asked for fifty".

The **When** column is one of:

* **Now** — before the fork exists, because after it the change is two changes.
* **At the fork** — into the new repo's defaults and manifests, where it costs nothing.
* **At the freeze** — needs the write freeze, or needs the re-key to have happened. This is also
  where the schema collapse to `0001_init.sql` lands, so schema shapes belong here.
* **Anytime** — genuinely independent of the split; listed because it is worth doing.

These are deliberately *not* phase numbers. The split plan was renumbered on 2026-09-05 when the
fork stopped coming first, and pinning fifty rows to a numbering that moves is how a survey rots.
Where a specific step genuinely matters, the prose names it; the **When** column never does.

## Working these in parallel

This list exists to be picked apart by several people (or agents) at once, so each numbered
section is written to stand alone: it names the files, states the current behaviour, and says
why the change belongs in this window. Reading the section should be enough; reading the whole
document should not be necessary.

Rules that keep concurrent work from colliding:

* **One item, one branch, one pull request.** Do not bundle items because they touch the same
  file. Items 3, 20 and 26 all live in the sync processors and are three different decisions.
* **Three clusters must land together, in one PR each.** They are not independently
  deployable:
  * **2 + 9 + 16** — the product claim. The audience→product map (9) is where the claim comes
    from, minting it is (2), enforcing it on scope is (16). Shipping the enforcement before the
    minting logs everybody out.
  * **7 + 8** — membership authorization. Refusing client-written `userId` without also
    refusing client-written `role` leaves the privilege escalation open, and vice versa.
  * **5 + 6** — the deploy workflow. Both edit `deploy.yml`'s auth and rollout steps and will
    conflict trivially.
* **Do not start an "at the freeze" item.** Items 4, 17, 26, 38 and 43 need the write freeze and
  the re-key to have happened. Writing them now produces a migration the freeze then has to undo.
  Design work on them is welcome; migrations are not.
* **Three items are decisions, not implementations**, and want a human answer before code:
  13 (which of the three numbers is the one that is right?), 14 (is the reaper armed?), 27
  (what does a partially committed sync mean to a client?). Bring the options, not a patch.
  Item 1 was a fourth until 2026-09-05, when it was settled: keep auto-migration, resequence
  the plan.
* **Item 33 changes the shape of the work** — a generated wire schema makes several of the
  others diffable across the two repos. If it is going to happen, earlier is better.
* **Amend this note rather than replacing it.** When an item lands, mark it and link the PR;
  when investigation shows an item is wrong or already handled, say so in place with the
  evidence. A struck-through item with a reason is worth more than a deleted one — the split
  plan's own §7 is the model.

---

## Summary

| # | Change | Score | When |
|---|---|--:|---|
| 1 | A collapsed `0001_init.sql` stops the fork booting — **landed** | 10 | Now |
| 2 | ~~JWT carries no product/audience claim~~ **landed** | 9 | Now |
| 3 | ~~Delete of an unknown id 500s the whole sync request~~ **landed** | 9 | Now |
| 4 | No foreign keys to `users`; deletion is 16 ordered DELETEs | 9 | At the freeze |
| 5 | `deploy.yml` uses a long-lived GCP service-account key | 9 | Now |
| 6 | Deployment pins `:latest`; rollout is `rollout restart` | 9 | Now |
| 7 | ~~Any list member can grant membership to any account~~ **landed** | 8 | Now |
| 8 | ~~`role` is client-supplied and gates list deletion~~ **landed** | 8 | Now |
| 9 | ~~Audience set is flat; no client-id → product mapping~~ **landed, with one gap** | 8 | Now |
| 10 | ~~Refresh tokens are Argon2-hashed~~ **landed** | 8 | Now |
| 11 | A Gemini HTTP call runs inside an open Postgres transaction | 8 | Anytime |
| 12 | ~~Probes point at the deprecated `/healthcheck`~~ **landed** | 8 | Now |
| 13 | Guardrail limits and the pod memory limit disagree by ~100× | 8 | Anytime |
| 14 | The retention reaper has never deleted anything | 8 | Now |
| 15 | No index on any tenancy column on the grocery/todo side | 8 | Now |
| 16 | ~~`SyncScope` is not bound to anything the caller proved~~ **landed** | 8 | Now |
| 17 | Tenancy columns are nullable | 7 | At the freeze |
| 18 | ~~`grocery_list_members.id` embeds the raw Google subject~~ **landed; `userId` still does** | 7 | Now |
| 19 | Sessions have no absolute lifetime | 7 | Anytime |
| 20 | One bad item fails the whole batch | 7 | Now |
| 21 | No caps on grocery/todo field sizes — ~~batch length~~ **landed** | 7 | Now |
| 22 | Deployment has no `securityContext` | 7 | Now |
| 23 | Valkey has no `maxmemory` or eviction policy | 7 | Now |
| 24 | `GEMINI_API_KEY` is `expect`ed at boot for a teddy.fyi-only feature | 7 | Now |
| 25 | The log-hashing privacy invariant holds in exactly one place | 7 | Anytime |
| 26 | Row ids are client-chosen and globally unique across accounts | 6 | At the freeze |
| 27 | Three sync futures, three independent transactions | 6 | Now |
| 28 | N+1 store-mapping queries inside the grocery transaction | 6 | Anytime |
| 29 | `delete_user_data` misses `list_join_failures` and the AI counters | 6 | Now |
| 30 | No session listing or "sign out everywhere" | 6 | Anytime |
| 31 | No `DELETE /api/devices/:id`, but there is a 10-device cap | 6 | Anytime |
| 32 | No per-account rate limit on `/api/*` | 6 | Anytime |
| 33 | No wire-contract artifact to diff the two repos against | 6 | Now |
| 34 | `replicas: 1`, no PodDisruptionBudget | 6 | Anytime |
| 35 | Prod runs on compiled-in defaults, two of which are wrong | 6 | Now |
| 36 | `require_auth` returns the JWT library's error text to the caller | 5 | Anytime |
| 37 | `LOG_HASH_SALT` is unset, so the salt is `JWT_SECRET` | 5 | Now |
| 38 | `stores`/`categories` are dual-scoped with no stated precedence | 5 | At the freeze |
| 39 | No per-account row quotas on grocery/todo tables | 5 | Anytime |
| 40 | Initial sync is unpaginated | 5 | Anytime |
| 41 | Nothing checks that the manifest and the code agree on env vars | 5 | Now |
| 42 | `LoginRequest.user_id` is accepted and ignored | 5 | Now |
| 43 | `sync_state` is an ENUM on two tables and TEXT on seven | 4 | At the freeze |
| 44 | Login is a two-statement, non-transactional upsert | 4 | Anytime |
| 45 | Rate limiting is per-process | 4 | Anytime |

---

## 1. A collapsed `0001_init.sql` stops the fork booting — **10**, Now — **landed**

**Landed 2026-09-05** in the split plan revision that reordered the phases: the plan now states
the auto-migration fact in its §1.3, keeps the collapse out of the fork (Phase 4 step 5), moves it
to the freeze (Phase 5 step 1), and carries the no-migrations constraint as Phase 4 step 10. The
analysis below is kept because the mechanics are what make those steps make sense.

`db::init_postgres` runs `sqlx::migrate!("./migrations").run(&pool)` on every start
(`src/db.rs:202`), and `init_app_state` calls it. The split plan used to say the opposite:

> Prod migrations are applied **by hand**. Nothing in `deploy.yml` or the binary runs
> `sqlx migrate run` against production… So the "two services racing migrations on boot"
> hazard does not exist here.

`deploy.yml` does not. The binary does, on every rollout restart, against whatever
`DATABASE_URL` names.

**But it is not a race, and the first version of this note was wrong to call it one.** Read
against sqlx 0.8.6 (`sqlx-core-0.8.6/src/migrate/migrator.rs`), `Migrator::run_direct` does two
things that matter here, both by default:

* `locking: true` — it takes an exclusive advisory lock before touching anything, so two pods
  cannot interleave. There is no race to lose.
* `ignore_missing: false` — `validate_applied_migrations` walks the applied set and returns
  `MigrateError::VersionMissing` for any version the running binary does not carry.

So the failure mode is a **crashloop, not corruption**, which is the good direction to fail in.
What it costs is the plan:

**Collapsing the fork's migrations while it still shares the database cannot work.** A collapse
to a single `0001_init.sql` in a binary pointed at the *existing* database, whose
`_sqlx_migrations` table holds eighteen rows and no version 1. The fork gets
`VersionMissing(20260610182740)`, `init_postgres` returns `Err`, and
`.expect("Failed to initialize PostgreSQL")` panics. The pod never serves.

The worse direction is the same mechanism pointed the other way. If the fork instead *keeps*
its history and later adds a migration of its own, that migration applies cleanly to the shared
database — and then `api-rust-dep`'s **next rollout restart** hits `VersionMissing` and
production teddy.fyi crashloops. A commit in the fork's repository takes down the original's
next deploy, which is the shared-image-tag hazard in a second costume.

### What to do

**Keep auto-migration.** It is the right shape for this service and for the destination the
plan is heading to: after the freeze there are two databases and two migration directories, each
binary owns its own, and the schema change ships in the same commit as the code that needs it —
which is the argument `AGENTS.md` already makes for keeping `k8s/` in this repo. Building a
separate migration job to serve a hazard that exists for a few days would be machinery to own
forever.

**Resequence instead.** Move the collapse to the freeze, where the fork creates its own database
and the single `0001_init.sql` is exactly right. Until then the fork carries the *identical*
files: checksums match, neither binary has anything to apply, and neither can surprise the other.

The price is one constraint, and it now sits in the plan explicitly: **no migrations in either
repo while two repos share one database.** A genuinely urgent one has to land in both repos
byte-identically — same filename, same content, same checksum — before either deploys. The
profile-first ordering shortens that window to the gap between the fork and the freeze, where the
fork-first ordering spanned two whole phases.

### The consequence that stays

`maxSurge: 1` with `maxUnavailable: 0` means the new pod runs its migrations while the old pod
is still serving the old code. Every migration must therefore be readable by the *previous*
release for the length of the rollout. That has been true all along and the existing migrations
mostly respect it — nullable adds, `IF NOT EXISTS`, and a `DELETE FROM` on a table whose rows
live ten minutes.

Phase 6 step 5 is where it stops being free: dropping `configs`, `drawings` and `devices` in the
same deploy that removes the code means the new pod drops the tables while the old pod, still
carrying the ScribbleRoute query paths, is serving requests against them. That step wants to be
**two deploys** — ship the code that no longer references the tables, wait for the rollout, then
ship the drop.

## 2. The JWT carries no product claim — **9**, Now — **LANDED**

**Landed** in [#64](https://github.com/toolateforteddy/teddy-fyi-api-rust/pull/64), together with items 9 and 16 — they are one change, as the
notes below said they would be. `Claims` now carries
`product: Option<Product>` (`src/auth/product.rs`), minted from the audience at login and from
the claiming parent's audience at device pairing, persisted on `sessions.product` so a refresh
re-mints it, and skipped from the JSON entirely when unknown so an older client's token still
decodes. Stage 1 of three: it *mints and tolerates*. Absence is permitted, because denying it
would sign out every device holding a token minted before the deploy. Stage 3 — making absence a
401 — is a later, deliberate change, and `a_token_without_a_product_claim_still_reaches_both_products`
fails if anybody brings it forward. The description below is what was there before.

`Claims` is `{sub, client_uuid, exp}` (`src/auth/tokens.rs:41`), and `require_auth` validates it
with a bare `Validation::new(Algorithm::HS256)` (`src/auth/middleware.rs:68`) — signature and
`exp`, nothing else. There is no `aud`, no `iss`, no product.

Split decision #3 keeps `JWT_SECRET` identical across the cutover and does not rotate it until
Phase 7. So from the traffic flip until decommissioning — weeks, by the plan's own pacing — an external
ScribbleRoute tester's access token is structurally valid at `api-rust.teddy.fyi`, and can drive
`scope: Grocery` writes into the household database. Today that is invisible because it is one
backend; the moment there are two hostnames it is a live cross-product boundary with nothing on
it.

The fix has to happen **before Phase 3** rather than during, because tokens live seven days:
both binaries need a build that mints the claim and tolerates its absence before either enforces
it. That ordering is the same shape as the legacy-token shim in the identity note §6, and it is
the reason this is a "now" item rather than a "later" one.

## 3. Deleting an unknown id 500s the entire sync request — **9**, Now

**Landed** in [#57](https://github.com/toolateforteddy/teddy-fyi-api-rust/pull/57). All ten
delete paths now acknowledge a delete for a row the server does not have, under one policy
written down in `src/routes/sync/deletes.rs`; `soft_delete_version!` owns the executor call so
`fetch_one` cannot be chosen for a delete again, and `src/routes/sync/tests/deletes.rs` fails
the build if it is. Two paths not named below were the same bug wearing a 403 —
`grocery_lists` ("Grocery list … not found") and `grocery_item_store_info` ("Parent grocery
item not found") — and three more (`grocery_items` and the config/drawing delta paths) said
nothing about the change at all, which leaves it pending on the device forever rather than
failing the batch. The description below is what was there before.

In `todo_items`, `todo_lists`, `categories`, `stores` and `grocery_list_members`, the
`OperationType::Delete` arm runs

```rust
let row = sqlx::query!("UPDATE … SET is_deleted = TRUE … RETURNING version", …)
    .fetch_one(&mut **tx).await?;
```

**outside** the `if let Some(row) = record` guard that precedes it — e.g.
`src/routes/sync/todo/todo_items.rs:251-283`. When the row does not exist, `fetch_one` returns
`RowNotFound`, `?` converts it to `AppError::Database`, and the caller gets a 500 with the whole
batch — todo, grocery *and* scribble futures — rolled back.

Reaching this is ordinary, not adversarial: a row created offline and deleted before it ever
synced, or a row a previous account deletion hard-removed. The client has no way to learn which
item did it, so it retries the same batch forever and that device stops syncing.

`grocery_items.rs:320-362` has the guard. Five of the seven processors do not. The divergence is
the point: this is copy-pasted authorization code that has already drifted, and after the fork
it is the same bug in two repositories.

## 4. No foreign keys to `users` — **9**, At the freeze

Nothing references `users(id)`. Not `sessions.user_id`, not any `"userId"`/`"ownerId"` column,
not `devices.user_id`, not `device_authorizations.user_id`. Migration `20260901120000` states
the reason honestly:

> No FK to "users": users.id is TEXT (the auth subject) while configs.user_id is a UUID derived
> from it via parse_or_hash_uuid, so the types genuinely do not line up.

The re-key removes that obstacle. Identity note §3 already spots the consequence — "`ON DELETE
CASCADE` … instead of eleven ordered deletes" — but the freeze's step list only specifies column
*types* and values, not constraints. Write the constraints into the target schema.

The prize is `delete_user_data` (`src/routes/user/deletion.rs:56-210`): sixteen hand-ordered
`DELETE` statements whose ordering is load-bearing, whose completeness nothing enforces, and
which item 29 below shows has already missed a table.

## 5. `deploy.yml` uses a long-lived GCP service-account key — **9**, Now

```yaml
permissions:
  id-token: 'write'     # set up for Workload Identity Federation
...
- uses: google-github-actions/auth@v2
  with:
    credentials_json: ${{ secrets.GCP_SA_KEY }}   # …and then not used
```

The step's own comment recommends WIF. The key it uses instead can push to GCR and `kubectl
apply` against the production cluster, and it does not expire.

The fork creates a second repository that needs the same access. Under the current scheme that
means copying a static, cluster-reaching credential into a second GitHub secret store — and the
new repo is the one the plan describes as "a repo whose contents you would not mind an outside
contributor reading". Switch to WIF **before** the fork exists, so the fork is provisioned with
its own federated identity rather than a copy of this one.

## 6. The Deployment pins `:latest` — **9**, Now

`deploy.yml` pushes both `:latest` and `:${{ github.sha }}`. `k8s/api-rust.yaml` names `:latest`.
The deploy step then runs `kubectl rollout restart deployment/api-rust-dep`, which re-pulls.
Three consequences:

* **No rollback.** Reverting means rebuilding, because nothing in the cluster records which
  commit is running.
* **No provenance.** `kubectl describe deploy` says `:latest` and nothing else.
* **The split plan's §1.3 hazard, in full.** A fork that merges to `main` overwrites the tag the
  running deployment resolves, and the next restart *anywhere* pulls the fork's binary into
  `api-rust-dep`. The plan calls this "the one that is easy to miss, and it is worse: it is
  silent."

Replacing the restart with `kubectl set image deployment/api-rust-dep api-rust=…:${{ github.sha }}`
fixes the rollback story and, as a side effect, makes the fork's push to `:latest` incapable of
changing what is running here. It is the cheapest available mitigation for the plan's own
worst-case, and it is one line.

## 7. Any list member can grant membership to any account — **8**, Now — **LANDED**

`process_grocery_list_member_changes` (`src/routes/sync/grocery/grocery_list_members.rs:155-182`)
writes `item.user_id` and `item.role` straight from the request body. The only gate is:

```rust
let is_member_of_target  = member_lists_set.contains(&item.list_id);
let is_member_of_current = existing_map.get(&change.id)
    .map(|row| member_lists_set.contains(&row.list_id))
    .unwrap_or(true);            // a fresh id is always "current-authorised"
```

So a member of list L can sync an insert with a fresh `id`, `listId = L`, and `userId` set to
anybody, and that account is now a member. The invite code, its TTL, the one-live-code-per-list
unique index, `max_outstanding_invites_per_user`, `MAX_INVITE_ATTEMPTS` and `list_join_failures`
are all optional — every one of them can be walked around by the sync endpoint.

The comment immediately above that code says what the intended rule is: *"Membership is granted
by `/api/lists/join` and by nothing else; sync only ever reflects a membership that already
exists."* The code does not implement that sentence. Make the member processor refuse to create
rows and refuse to change `userId`, and let `join_handler` be the only writer.

**Landed together with item 8** ([#56](https://github.com/toolateforteddy/teddy-fyi-api-rust/pull/56)). The insert/update path in
`process_grocery_list_member_changes` no longer upserts: it looks the row up by id and refuses
anything that is not already there, so `join_handler` and the list-creation seed in
`grocery_lists.rs` are the only writers of a membership. `"listId"`, `"userId"`, `role` and
`"joinedAt"` are gone from the statement entirely — a payload that disagrees about the first two
is refused rather than applied, and un-deleting a membership is refused too, since that is a
grant by another name. The one accommodation is the offline-first batch: a client that created a
list offline and invented a local membership row for *itself* gets a no-op plus the server's
canonical row echoed back, rather than a 403 on every list it makes.

## 8. `role` is client-supplied and gates list deletion — **8**, Now — **LANDED**

Same upsert, `role = EXCLUDED.role`. And `grocery_lists.rs` reads it:

```rust
let is_owner = existing_list.and_then(|l| l.owner_id.as_deref()) == Some(user_id)
    || member_row.role == "OWNER";
```

A member of a shared list can sync their own membership row with `role: "OWNER"` and then delete
the list, which soft-deletes every item, store and category on it for the whole family. This is
the only place `role` is read today, which is what makes it survivable — and exactly why it
should be fixed before anyone adds a second reader. `role` should be server-assigned, and it
should be in the "never taken from the payload" set alongside `userId`.

**Landed with item 7** ([#56](https://github.com/toolateforteddy/teddy-fyi-api-rust/pull/56)). `role` is now server-assigned: sync ignores what
the payload says and echoes the stored role back as a remote change so the client stops
disagreeing. `test_sync_member_cannot_promote_self_to_owner` pins both halves — the promotion is
dropped, and the list delete it existed for is still refused.

## 9. The audience set is flat — **8**, Now — **LANDED, with the gap this item predicted**

**Landed** in [#64](https://github.com/toolateforteddy/teddy-fyi-api-rust/pull/64). `ClientCatalog` (`src/auth/client_ids.rs`) is the
`client id → Option<Product>` map, read from two new per-product vars — `TEDDY_FYI_CLIENT_IDS`
and `SCRIBBLEROUTE_CLIENT_IDS` — plus the legacy vars, two of which classify themselves by name
(`GOOGLE_CLIENT_ID_GROCERY_WEB` → teddy.fyi, `SCRIBBLEROUTE_API_CLIENT_ID` → ScribbleRoute). An
ID claimed by both products refuses to start.

**The gap is the one this item named, and it is not closed by code.** `GOOGLE_IOS_CLIENT_IDS`
still holds both products' iOS apps in one secret and nothing in the repo says which is which,
so those IDs — and `GOOGLE_CLIENT_ID` — are accepted, unclassified, and their sessions carry no
product claim. Guessing was rejected deliberately: a wrong guess denies a real device its own
scopes, which is an outage wearing a security feature's clothes. Closing it is **configuration,
not a deploy** — list each ID under one of the two per-product vars. The pod logs the
unclassified set at boot (a `warn` naming every one of them) so the remaining work is visible
from a rollout rather than from reading source. Until then item 16's enforcement applies only to
sessions established through a classified ID.

The description below is what was there before.

`load_google_client_ids` (`src/auth/client_ids.rs`) unions `GOOGLE_CLIENT_ID`,
`GOOGLE_CLIENT_ID_GROCERY_WEB`, `SCRIBBLEROUTE_API_CLIENT_ID`, `GOOGLE_CLIENT_IDS` and
`GOOGLE_IOS_CLIENT_IDS` into one `HashSet<String>`, and `login_handler` checks membership. The
set does not record which client belongs to which product.

The split plan's Phase 1 step 6 now narrows the audience set **per profile**, which pulls this
forward: the product profile cannot pick the right ids without a map, so this is a prerequisite of
that step rather than a tidy-up at the fork. But `GOOGLE_IOS_CLIENT_IDS` is a comma-separated
secret containing *both* products' iOS apps, and nothing in the repo says which id is which. That
step cannot be executed correctly from the code alone.

Turn the set into a `HashMap<ClientId, Product>` now, while both halves are still in one place
and one person's head. It is also the prerequisite for item 2 — the audience is where the product
claim comes from.

## 10. Refresh tokens are Argon2-hashed — **8**, Now — **LANDED**

**Landed** in [#70](https://github.com/toolateforteddy/teddy-fyi-api-rust/pull/70).
`hash_refresh_token` is now a domain-separated SHA-256 (`teddy-fyi/refresh-token/v1:`), the same
idiom `hash_device_code` already used, and `verify_refresh_token` reads **either** format so no
device is signed out.

There is no migration, and that is the interesting part: a stored Argon2 digest cannot be
recomputed as a SHA-256 — the only input that could do it is the token itself, which the server
has never stored. `20260905130000` could delete its rows because a device code lives ten minutes;
a session lives seven days and deleting them signs out the estate. So the *write* side migrates
instead: every mint and every rotation stores the new form, a session upgrades on its first
refresh, and the Argon2 branch drains on its own within the window `expires_at` already bounds.
It is deletable once `SELECT count(*) FROM sessions WHERE refresh_token_hash LIKE '$argon2%'` is
zero.

The `.expect("invalid hash format")` is gone — an unparseable stored hash is now a mismatch and
an `error!` line, not a panic — so the `guardrails` module docs and its panic test no longer
claim a live example that does not exist. `CatchPanicLayer` stays: "no handler will ever panic"
is not a claim that survives future changes.

The description below is what was there before.

`hash_refresh_token` / `verify_refresh_token` (`src/auth/tokens.rs:85-96`) use `Argon2::default()`
— Argon2id at ~19 MiB and tens of milliseconds. A refresh token is 64 characters of CSPRNG
`Alphanumeric`: no guessable structure, no low-entropy human part. A memory-hard KDF protects
nothing that 380 bits of randomness does not already protect.

Migration `20260905130000_device_code_hash_sha256.sql` makes this argument at length, for a
credential of identical shape, and moves device codes to a domain-separated SHA-256. Refresh
tokens were left behind. What it costs:

* `/auth/refresh` pays up to **three** Argon2 operations per call (verify current, verify old,
  hash new) on a path a device hits ~100×/day now that `ACCESS_TOKEN_TTL_SECS` is 900. Against a
  256Mi pod (item 13) that is the largest single allocation in the request path.
* `verify_refresh_token` calls `PasswordHash::new(hash).expect("invalid hash format")` — the panic
  the `guardrails` module docs cite as the reason `CatchPanicLayer` exists at all. A deterministic
  hash makes that `.expect` disappear rather than needing a guard.

Do it before the split: it is a stored-format change on `sessions`, and the freeze copies
`sessions` rows.

## 11. A billed HTTP call inside an open transaction — **8**, Anytime

`process_todo_changes` (`src/routes/sync/todo/todo_items.rs:106-125`) calls `charge_gemini_call`
(Redis) and then `assign_todo_icon` (a 12-second HTTP budget) **while holding the todo
transaction** and the pool connection under it. The pool is 16 connections
(`DEFAULT_API_MAX_CONNECTIONS`). Two dozen concurrent syncs carrying icon-less todos and every
slot is held by a future waiting on Google.

The rest of this codebase is careful about exactly this — the AI budget module, the outbound
timeout sized below the request deadline, the shared `reqwest` client. This one call site sits
inside a transaction anyway.

## 12. The probes point at the deprecated endpoint — **8**, Now — **LANDED**

**Landed** in [#73](https://github.com/toolateforteddy/teddy-fyi-api-rust/pull/73).
Readiness now probes `/healthz/ready`, so `src/observability/health.rs` is load-bearing in
production for the first time. Liveness probes `/healthz/live` and stays a static string
deliberately: liveness failure means "kill this pod", and a liveness check on a *shared*
dependency lets one Redis outage restart every replica at once.

The `BackendConfig` went to `/healthz/live` rather than `/healthz/ready`, which is the one
judgement call in the change. That is the Google load balancer's own check — a 30-second
interval with a 25-second timeout, and GKE is slow to return a backend it has marked
unhealthy — so pointing it at a dependency check would let seconds of Redis trouble remove
the backend for minutes after Redis recovered. It loses nothing: `api-rust-svc` is a
NodePort and kube-proxy only forwards to *ready* pods, so when readiness takes the pod out
there is nothing behind the node port for the load balancer to reach either.

`readinessProbe.failureThreshold` is now spelled out rather than left to its default of 3,
because with `replicas: 1` (item 34) the number is load-bearing: three failures ten seconds
apart is what keeps a momentary blip from pulling the only replica out of rotation.

**`/healthcheck` is still in `main.rs`, for one more deploy.** The kubelet picks up the new
pod template as pods are replaced, but the `BackendConfig` path reaches the load balancer's
probers through the GCP API asynchronously — minutes, not milliseconds. An image that had
already dropped the route would spend that window failing every load-balancer check, which
is a 502 for everyone. Deleting it is a follow-up once a deploy carrying this change has
been observed healthy.

Also landed: a test (`observability::tests::manifest_probe_paths`) that reads the three
probe paths out of `k8s/api-rust.yaml` and drives each through the real router, failing on
a 404. It is what would have caught this item without anyone reading the manifest, and it
fails if the manifest is pointed back at `/healthcheck`.

`/api/ready` is left where it is. Item 12 notes it sits behind `require_auth` and is
therefore unreachable by a probe; that is deliberate and documented — it is the deep
Postgres check, kept for on-demand human use, and Neon bills per wake-up, so a probe on a
timer would keep the database awake around the clock to answer monitoring.

The description below is what was there before.

Readiness, liveness *and* the GKE `BackendConfig` health check all target `/healthcheck`, which
`src/main.rs:328` describes as:

> Superseded by `/healthz/live`. Kept until the cluster's probes have been repointed, so this
> deploy cannot strand a rollout; delete after.

They were never repointed. So `src/observability/health.rs` — 355 lines built to answer "can this
replica actually serve", with a held Redis connection, a cached verdict and a cap on the failure
path — is dead in production, and liveness is `get(|| async { "OK" })`: a constant string that
stays green through any dependency failure the process survives.

Phase 2 stands up the second deployment's manifests and Phase 4 hands them to the fork, so both
inherit this. Also
note `/api/ready`, the deep readiness check with a real database ping, sits behind `require_auth`
and is therefore unreachable by a probe.

## 13. The guardrails and the memory limit disagree by ~100× — **8**, Anytime

* `DEFAULT_MAX_CONCURRENT_REQUESTS` = 512 (`src/guardrails.rs`)
* `DEFAULT_MAX_BODY_BYTES` = 8 MiB, and `AppJson::from_request` buffers the whole body into
  `Bytes` before parsing (`src/routes/sync/types.rs:421`)
* `k8s/api-rust.yaml`: `limits.memory: 256Mi`, one replica

512 × 8 MiB is 4 GiB of admissible in-flight body against a 256Mi cgroup, before the ~19 MiB per
concurrent Argon2 from item 10. The load-shed layer sheds at a depth the pod cannot reach, so the
real failure mode is OOMKill — which `CatchPanicLayer` cannot catch, which takes every in-flight
request with it, and which on a single-replica deployment is an outage.

Three numbers, at least one of which is wrong. Pick the pod size first and derive the other two.

## 14. The retention reaper has never deleted anything — **8**, Now

`k8s/user-reaper.yaml` sets `REAP_DRY_RUN: "true"`, and `parse_dry_run`
(`src/jobs/reap_stale_users.rs:48`) treats anything but a literal `"false"` as dry-run. The
policy published at scribbleroute.com/privacy promises erasure within 30 days of an account
passing 12 months of inactivity.

Split decision #4 moves this job to the ScribbleRoute repo and drops its eligibility workaround,
on the assumption that it works. Nobody has watched it commit. Arm it — or at minimum read a
dry-run log and confirm the eligible set is the one you expect — before it becomes somebody
else's repository's problem.

## 15. No index on any tenancy column, grocery/todo side — **8**, Now

Every index in `migrations/` covers `listId`, `client_uuid`, `device_uuid`, `expires_at`, or a
failure counter. There is nothing on:

`todo_items."userId"` · `todo_lists."userId"` · `grocery_items."userId"` ·
`grocery_lists."ownerId"` · `grocery_list_members."userId"` · `stores."userId"` ·
`categories."userId"`

Those are the columns every download query and every `/api/sync/status` fallback filters on —
including the eight-way `UNION ALL` in `status.rs:70-100`, which sequentially scans six tables.
`grocery_list_members."userId"` alone appears in six of those arms.

Add `("userId", updated_at)` composites. It is a twenty-line migration, it is a straight latency
win today, and after the fork it is two migrations in two repos with two review cycles.

## 16. `SyncScope` is not bound to anything the caller proved — **8**, Now — **LANDED**

**Landed** in [#64](https://github.com/toolateforteddy/teddy-fyi-api-rust/pull/64), with items 2 and 9. `authorize_scope`
(`src/routes/sync/scope_auth.rs`) runs at the top of both readers of `scope` — `POST /api/sync`
and `GET /api/sync/status` — before a transaction is opened or the status cache is read, and
403s a scope that does not belong to the token's product. `SyncScope::product()` is exhaustive,
so a new scope will not compile until somebody says which side of the split it lands on.
`All` is teddy.fyi's, which is a description of what the handler already does rather than a new
restriction — and it closes the "omit the field and get the default" route around the check.
A token with no product claim still reaches everything; see item 2 on why, and on what
deleting that test would mean. The description below is what was there before.

`let scope = payload.scope.unwrap_or(SyncScope::All);` (`src/routes/sync/handler.rs:30`). The
scope is a body field. With item 2 unfixed, any valid token reaches all six scopes and both
products' tables.

This is the enforcement half of item 2 and should land with it: derive the permitted scope set
from the token's product claim, and 403 anything outside it. It is also the change that makes the
split plan's §1.1 claim ("the seam that is already clean") true at runtime rather than only by
convention.

## 17. Tenancy columns are nullable — **7**, At the freeze

`todo_lists."userId"`, `todo_items."userId"`, `grocery_lists."ownerId"`, `grocery_items."userId"`,
`stores."userId"`, `categories."userId"` and `grocery_item_store_info."userId"` are all `TEXT`
NULL. A row with a NULL there is:

* unreadable — every fetch is `WHERE "userId" = $1`, which never matches NULL;
* unwritable — every authorization check is `row.user_id.as_deref() != Some(user_id)`, which is
  true for NULL, so the write 403s;
* **un-erasable** — `delete_user_data` matches on equality too.

That is permanently orphaned user data that no erase path can reach, which is the same category
of problem the log-hashing in `observability::http` exists to avoid. Count them, decide what they
are, and make the columns NOT NULL. The freeze is the window; the re-key rewrites these columns
anyway.

## 18. Membership row ids embed the raw Google subject — **7**, Now — **LANDED, and incomplete**

**Landed** in [#PRNUM](https://github.com/toolateforteddy/teddy-fyi-api-rust/pull/PRNUM).
Both writers — `/api/lists/join` and the creator's ADMIN row in the list sync processor —
now mint `gen_random_uuid()`.

**The derivation was load-bearing, which the item did not say.** Both writers upsert
`ON CONFLICT (id)`: that is how re-joining a list you left revives your row instead of
adding a second one, and it only worked because the id was derivable from the pair. Taking
the derivation away without replacing the conflict target would have produced two
membership rows per person per list. Migration `20260909120000` moves the constraint to
where it belonged — a unique index on `("listId", "userId")` — and both upserts now conflict
on the pair. It also drops the plain `("listId", "userId")` index from `20260908120000`,
which the unique one supersedes, and de-duplicates first: rows predating PR #56 could have
been created by sync with any client-chosen id, so a pair may have more than one row in a
database that has been running since June.

Existing rows **keep their old ids**. Rewriting a client-visible primary key underneath
devices that hold it, with no tombstone for the old value, is a ghost row on every phone in
the household; the pair-matched upsert finds and revives the old row instead.

**The disclosure is not actually closed, and this is worth knowing before ticking the item
off.** `GroceryListMemberData.user_id` carries the raw subject to every co-member on every
sync (`remote_mutations.rs:70-100`), independently of the id. The id was one of two
channels and this closes the one the item named. Closing the other is a **wire-contract
change** — clients read `userId` to identify who a membership row is for — so it needs the
two client families in the loop, which is precisely the coordination this window exists to
avoid. Worth its own item; it is cheaper now than after the fork, for exactly the reason
given below.

The description below is what was there before.

`join_handler` builds `format!("{}-member-{}", list_id, user_id)`
(`src/routes/lists/handlers.rs:276`), and that id syncs verbatim to every member of the list. So
joining a shared grocery list discloses your Google `sub` to everyone else on it.

The derivation note records this and identity note §10 says the teddy.fyi side keeps raw subjects
after the split — which is fine as a *storage* decision and not fine as a *disclosure* one. Make
the id a `gen_random_uuid()`. Do it now: it is a client-visible primary key, so after the fork it
is a coordinated change across two backends and two client families instead of one.

## 19. Sessions have no absolute lifetime — **7**, Anytime

`refresh_handler` writes `expires_at = now() + 7 days` on every rotation
(`src/auth/handlers.rs:544`). A device that refreshes stays signed in forever; `sessions.created_at`
is recorded and never read. Combined with the 15-minute access token this is an indefinite
credential with a 7-day idle timeout and no re-authentication, on a service whose users are
children's tablets that leave the house.

`ACCESS_TOKEN_TTL_SECS` has a long, careful comment about the sign-out window. The session behind
it has no ceiling at all.

## 20. One bad item fails the whole batch — **7**, Now

Every processor does `return Err(AppError::Forbidden(...))` or `return Err(AppError::Serialization(...))`
on a single item, which aborts that entity's transaction and — through `try_join!` — the request.
`SuccessResult` / `upload_status` exist to report per-item outcomes, but no path ever reports a
per-item *failure*: an item either succeeds or takes the request down with it.

The practical effect is the same as item 3: one unauthorized or unparseable row wedges a device's
sync loop with no way for the client to identify or skip it.

`validate_sync_payload` makes the opposite choice explicitly and argues for it ("an all-or-nothing
400 naming the offending item is something it can act on") — but that reasoning depends on the
error naming the item, and `AppError::Forbidden` from inside a processor does while
`AppError::Database` from item 3 does not. Decide the error model once, here, before it is two
implementations that drift.

## 21. No caps on grocery/todo payloads — **7**, Now — **half landed**

**The batch-length half landed** in [#59](https://github.com/toolateforteddy/teddy-fyi-api-rust/pull/59):
`SyncLimits` now bounds how many items one sync body may carry, per collection and in total
(`DEFAULT_MAX_ITEMS_PER_COLLECTION` / `DEFAULT_MAX_ITEMS_TOTAL` in
`src/routes/sync/limits.rs`). **The per-field size half is still open** — the `TEXT` columns
below remain unbounded. The description below is what was there before.

`validate_sync_payload` (`src/routes/sync/limits.rs:131`) bounds `drawings[]` and `configs[]` and
nothing else. On the teddy.fyi side:

* `todo_items.description`, `grocery_items.notes`, and every `name` are unbounded `TEXT`;
* nothing caps how many change deltas one request may carry. An 8 MiB body of small deltas is on
  the order of 10⁵ items, each of which does its own queries inside one transaction (item 28).

The ScribbleRoute half of this service has per-item bounds, device quotas and an AI budget. The
teddy.fyi half has none — and the teddy.fyi half is what stays in this repo. The asymmetry is
worth closing while the good patterns are still in the same tree as the code that needs them.

## 22. No `securityContext` on the Deployment — **7**, Now

The Dockerfile creates a fixed uid 10001, chowns the binary to root, chmods it 755, and says why:

> A fixed uid/gid (not a distro-assigned one) keeps it stable across base image rebuilds and lets
> a Kubernetes securityContext pin `runAsUser: 10001` without reading the image first.

`k8s/api-rust.yaml` never does. No `runAsNonRoot`, no `runAsUser`, no `readOnlyRootFilesystem`
(the process writes nothing to disk — the Dockerfile says so), no `allowPrivilegeEscalation: false`,
no `capabilities: drop: [ALL]`, no `seccompProfile`. The image did the work; the manifest never
claimed it. The fork inherits the manifest.

## 23. Valkey has no `maxmemory` — **7**, Now

`k8s/cache.yaml` gives the container `limits.memory: 128Mi` and passes Valkey no configuration at
all. With no `maxmemory` and no `maxmemory-policy`, it grows into the cgroup limit and is
OOMKilled by the kernel rather than evicting keys. There is currently no eviction path.

The split plan names eviction pressure as one of the three real shared-Redis risks. Set
`maxmemory` below the container limit and `maxmemory-policy allkeys-lru` — everything in there
(sync watermarks, AI counters) is reconstructible, which is what makes LRU correct here.

Now rather than later, because Phase 2 stands up a second Valkey cold and empty — the worst
moment to discover the memory behaviour — and Phase 4 duplicates this file into the fork.

## 24. `GEMINI_API_KEY` is `expect`ed at boot — **7**, Now

`std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY must be set")` (`src/main.rs:87`), for a
feature only teddy.fyi uses. The split plan's risk register carries an entry for this:

> Pod will not boot after removing Gemini — `init_app_state` `expect`s `GEMINI_API_KEY`; remove
> code, `AppState` field and manifest env together.

Making it an `Option<String>` and 503-ing `/categorize` and `/assign-icon` when it is absent turns
a three-way simultaneous edit into a no-op, and deletes a line from the risk register. Half an
hour.

## 25. The log-hashing invariant holds in one place — **7**, Anytime

`observability::http` hashes user ids out of the request log, with a good argument:

> Cloud Logging is outside the reach of both `DELETE /api/user/data` and
> `jobs::reap_stale_users`, so a raw id in the logs would be a copy of user-identifying data that
> neither erasure path can reach.

Then roughly twenty-five other `tracing::` call sites log the raw Google subject.
`refresh_handler` alone has twelve (`src/auth/handlers.rs:329-580`), plus `auth/device.rs`,
`jobs/reap_stale_users.rs:165,191,201`, `routes/user/handlers.rs:28`, and
`routes/user/deletion.rs:285`, which logs the whole `user:{sub}:last_update:{scope}` cache key by
name.

Either the invariant is real — in which case it needs a newtype or a lint, not a convention — or
it is not, in which case the hashing is buying complexity for nothing. Worth resolving before the
answer has to be maintained in two repositories with two privacy postures (identity note §10 is
explicit that they diverge).

## 26. Row ids are client-chosen and globally unique — **6**, At the freeze

`todo_items.id`, `todo_lists.id`, `grocery_lists.id`, `stores.id`, `categories.id` are TEXT
primary keys taken from the request body, in a single namespace shared by every account. The
ownership checks mean this is not a data-disclosure hole, but:

* any authenticated account can permanently squat an id, after which the rightful owner's insert
  hits `ON CONFLICT (id)`, fails the ownership check, and 403s their entire batch (item 20);
* 403-vs-200 is an existence oracle over the id space.

Scoping the primary key to `(user_id, id)` is a schema change that needs a freeze.

## 27. Three futures, three transactions — **6**, Now

`tokio::try_join!(todo_future, grocery_future, config_drawing_future)`
(`src/routes/sync/handler.rs:511`). Each opens its own transaction and commits independently, so
a `scope: All` request can commit todo, fail grocery, and answer 500 — leaving the client unable
to tell what landed. It also borrows three of sixteen pool connections per request.

Worth deciding deliberately now, because the fork keeps one of the three arms and this repo keeps
two, and whichever semantics you pick will be much harder to change once the code is in two
places.

## 28. N+1 inside the grocery transaction — **6**, Anytime

`process_grocery_changes` runs a `SELECT DISTINCT … JOIN grocery_items … JOIN grocery_list_members`
plus one insert per returned mapping, **per item**, inside the transaction
(`src/routes/sync/grocery/grocery_items.rs:226-263`). The join predicate is `LOWER(gi.name) =
LOWER($1)`, which no index covers. A hundred-item shop is a hundred of these.

## 29. `delete_user_data` misses two things — **6**, Now

`src/routes/user/deletion.rs` deliberately reaches `device_claim_failures` — the comment explains
that it carries the auth subject and so is "a user-identifying row an erase has to reach". It does
not touch `list_join_failures`, which migration `20260906113742` created with the identical shape
and the identical comment ("Mirrors `device_claim_failures` down to the column names"). It also
leaves the account's `ai:gemini:calls:user:{id}:{day}` Redis counters behind, though it does clear
the six `last_update` keys.

Small, but it is the concrete evidence for item 4: a hand-maintained list of sixteen deletes has
already fallen one behind the schema, silently, within a day of the table being added.

## 30. No session listing, no "sign out everywhere" — **6**, Anytime

`logout_handler` ends only the session named by the token presented. There is no way to enumerate
an account's live sessions and no way to end them all. `GET /api/devices` shows the hardware but
not the credentials, and `sessions` is keyed `(user_id, client_uuid)` — the data is right there.
The only bulk revoke is `DELETE /api/user/data`, which also erases the drawings.

## 31. A 10-device cap with no way to free a slot — **6**, Anytime

`src/routes/devices/limits.rs` argues carefully against automatic eviction:

> A family at the cap should remove a device they no longer use — a visible act — and the number
> is set high enough that this is rare.

`GET`, `POST` and `PATCH /api/devices/:id` exist. `DELETE` does not. There is no way to perform
the visible act.

## 32. No per-account rate limit on `/api/*` — **6**, Anytime

`tower_governor` is applied to the `/auth` router only (`src/main.rs`). `/api/sync` — three
transactions, an unbounded batch, N+1 queries, and an outbound AI call — is bounded by nothing
except the global 512-request concurrency limit. The Gemini spend and the SSE stream count are
metered per account; request rate is not.

## 33. No wire-contract artifact — **6**, Now

The Android and iOS payload shape is defined by `serde` attributes on `SyncRequest`/`SyncResponse`
and by the test suite. The split plan accepts protocol drift between the two repos — reasonably —
but accepted drift with nothing to diff against means the first divergence is discovered by a
tablet in a house.

`schemars` is already a dependency. Generating a JSON Schema for the sync request/response and
checking it into both repos costs an afternoon and makes drift a diff.

## 34. One replica, no PodDisruptionBudget — **6**, Anytime

`replicas: 1`, `maxUnavailable: 0`, and `topologySpreadConstraints` with `DoNotSchedule` — so a
rollout blocks until a second node has room, and a node drain takes the service down with nothing
to object. Worth noting alongside: the per-IP rate limiter buckets and the `SSE_MAX_STREAMS_TOTAL`
ceiling are per-process, so both are silently multiplied the day this becomes 2.

## 35. Prod runs on compiled-in defaults, two of which are wrong — **6**, Now

`AGENTS.md` states it plainly: ~40 environment variables read, 11 set, "the default in the Rust
source *is* the production value". That is a defensible starting point, but two of the unset ones
are wrong for production *today*:

* `COOKIE_DOMAIN` defaults to `.teddy.fyi`, so `session_cookie` emits `Domain=.teddy.fyi` on
  responses served from `api.scribbleroute.com`, which the browser discards. The split plan
  already flags this as a latent bug.
* `CORS_ALLOWED_ORIGINS` defaults to a list carrying both products.

Setting them explicitly before the fork means the fork inherits a manifest that *states* its
configuration rather than one that relies on the source it is about to edit.

## 36. `require_auth` returns the JWT library's error to the caller — **5**, Anytime

```rust
axum::Json(serde_json::json!({ "error": format!("Invalid token: {}", err) }))
```

(`src/auth/middleware.rs:82`) distinguishes `ExpiredSignature` from `InvalidSignature` from
`InvalidToken` for an unauthenticated caller. `refresh_error` in the adjacent module went to
considerable length to collapse exactly this kind of oracle down to two codes; the middleware next
to it did not.

## 37. `LOG_HASH_SALT` is unset, so the salt is `JWT_SECRET` — **5**, Now

`log_hash_salt` falls back to `JWT_SECRET` (`src/observability/http.rs:61`). Phase 7 rotates
`JWT_SECRET` on the ScribbleRoute side; on that day every historical `user_hash` in Cloud Logging
becomes uncorrelatable with every future one, and the log-based metrics the observability roadmap
built on them silently discontinue.

Set `LOG_HASH_SALT` explicitly in the manifest now. One line, and it also lets the two products'
log hashes be deliberately different (or deliberately the same) rather than incidentally coupled
to a secret with its own rotation schedule.

## 38. `stores` and `categories` are dual-scoped — **5**, At the freeze

Both carry a nullable `"userId"` *and* a nullable `"listId"`, and the authorization checks OR them
(`src/routes/sync/grocery/stores.rs:248-258`). A store can belong to a user, to a list, to both,
or to neither, and nothing states which wins. `delete_user_data` deletes on `"userId" = $1 OR
"listId" = ANY($2)`, which is the only place the ambiguity is resolved — by deleting on both.

## 39. No per-account row quotas on grocery/todo — **5**, Anytime

`MAX_DEVICES_PER_ACCOUNT`, `SSE_MAX_STREAMS_PER_USER`, `GEMINI_MAX_CALLS_PER_USER_PER_DAY` and the
invite caps all exist. There is no equivalent bound on how many `todo_items` or `grocery_items` an
account may hold. Same asymmetry as item 21.

## 40. Initial sync is unpaginated — **5**, Anytime

`last_synced_at = None` returns every row the account owns in a single response
(`fetch_remote_todo_mutations` and friends have no `LIMIT`). The 8 MiB guardrail bounds the request
only; the response is unbounded. Fine for a household, a cliff for anything else, and it is the
first request every reinstalled client makes.

## 41. Nothing checks the manifest against the code — **5**, Now

No test, lint or startup assertion relates the env vars `k8s/api-rust.yaml` sets to the ones the
binary reads. A variable removed from the code and left in the manifest is invisible; a variable
added to the code and forgotten in the manifest is a crash-loop (item 24 is the example the risk
register already records).

A startup line that dumps the resolved configuration, plus a test asserting the manifest's env
keys are a subset of a known list, is an afternoon — and it is worth more after the split, when
the two manifests can diverge from each other as well as from their code.

## 42. `LoginRequest.user_id` is accepted and ignored — **5**, Now

`LoginRequest` carries `user_id` (`src/auth/handlers.rs:83`). In the production path it is
discarded — identity comes from the validated Google token. In the `dev-auth` path it *is* the
identity. So one field is simultaneously inert and fully trusted depending on a compile-time
feature.

Identity note §7 establishes that `user_id` on `RefreshRequest` is genuinely load-bearing and
cannot be dropped. That makes this field, with the same name and the opposite trust level, exactly
the one a future reader will confuse. Remove it from `LoginRequest` (the dev bypass can take a
differently-named field) before the re-key makes the distinction subtle.

## 43. `sync_state` is an ENUM on two tables, TEXT on seven — **4**, At the freeze

`configs` and `drawings` use the Postgres `sync_state` enum. Every grocery/todo table stores the
same four values as free `TEXT DEFAULT 'SYNCED'`. Phase 6 step 5 proposes dropping the enum "if
nothing else uses it"; the honest position is that the two halves of this schema have disagreed
since migration `20260623120000`. Pick one per repo and stop carrying both.

## 44. Login is two statements, not a transaction — **4**, Anytime

`issue_session` (`src/auth/handlers.rs:147-179`) upserts `users` and then upserts `sessions`, both
directly on the pool. A failure between them leaves a `users` row with no session. Harmless today
— the next login fixes it — but the freeze's copy program selects its subset by reading `users` and
`sessions` together, so a population of session-less rows is one more thing to explain during a
freeze.

## 45. Rate limiting is per-process — **4**, Anytime

`tower_governor` buckets live in one pod's memory (`src/rate_limit/auth_limits.rs`), so the
effective limit is `burst × replicas` and a rollout resets every bucket. Redis is already a
dependency and already the store for the AI budget. Low priority at one replica; worth knowing
before the replica count changes.

---

## What is deliberately not here

* **Anything the split plan or the identity note already covers** — the `COOKIE_DOMAIN` fix
  (except as item 35's manifest half), the k8s resource renames, the audience narrowing, the
  `parse_or_hash_uuid` rename. Those are in the plan's Phases 1, 4 and 5 already.
* **Test coverage.** The suite is substantial (roughly a third of the tree) and its shape —
  characterisation tests that are *supposed* to go red, log-hygiene assertions, a dev-auth
  build matrix — is better than the code it covers in several places. There is no gap here
  worth a line item against the ones above.
* **The Cargo workspace idea.** Option E in the split plan, correctly rejected there.
