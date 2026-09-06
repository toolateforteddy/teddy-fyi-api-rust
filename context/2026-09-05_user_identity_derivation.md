# Two user identities: the raw auth subject and the UUID derived from it

Written 2026-09-05, out of a security review. **Nothing in this note has been changed in the
code.** The behavioural fix was deliberately deferred — re-keying the derivation would orphan
every config, drawing and device row that exists — so the decision was to write the design down
properly first and cost the migration separately. This file is the record of what is actually
true today.

Companion docs: [AGENTS.md](../AGENTS.md), [2026-07-10_project_context.md](2026-07-10_project_context.md),
[2026-09-04_device_pairing_auth.md](2026-09-04_device_pairing_auth.md).

## The one function

`src/routes/sync/remote_mutations.rs`:

```rust
pub fn parse_or_hash_uuid(s: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(s).unwrap_or_else(|_| {
        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, s.as_bytes())
    })
}
```

Given a string, hand back a UUID: parse it if it already is one, otherwise hash it. It is called
on three different kinds of input, and only the first is an identity:

| Input | Where it lands |
| :-- | :-- |
| the auth subject (`Claims.sub`) | `configs.user_id`, `drawings.user_id`, `devices.user_id` |
| `client_id` from the sync request | `configs.client_uuid`, `drawings.client_uuid` — an echo-suppression tag, not identity |
| a config/drawing change id from the client | `configs.id`, `drawings.id` — the row's own primary key |

The mapping is **stored nowhere**. It is recomputed from the subject on every request, which is
exactly why changing it is a migration rather than a refactor.

## 1. There are two user identities in one system

The same signed-in person is named two different ways in the same database, and which one applies
depends only on which table you are touching.

**Raw subject, stored as `TEXT`.** The Google `sub` verbatim, straight off `Claims.sub`:

| Table | Column | Migration |
| :-- | :-- | :-- |
| `users` | `id` (PK) | `20260615100000_create_users_table.sql` |
| `sessions` | `user_id` (PK half) | `20260611120000_create_sessions.sql` |
| `todo_lists`, `todo_items` | `"userId"` | `20260610182740_init_schema.sql` |
| `grocery_lists` | `"ownerId"` | same |
| `grocery_list_members` | `"userId"` | same |
| `stores`, `categories`, `grocery_items`, `grocery_item_store_info` | `"userId"` | same |
| `list_invites` | `"createdBy"` | `20260624143000_collaborative_lists_and_invites.sql` |
| `device_authorizations`, `device_claim_failures` | `user_id` | `20260904120000_create_device_authorizations.sql` |

**Derived UUID, stored as `UUID`.** `parse_or_hash_uuid(sub)`:

| Table | Column | Migration |
| :-- | :-- | :-- |
| `configs` | `user_id` | `20260623120000_create_syncable_configs_drawings.sql` |
| `drawings` | `user_id` | same |
| `devices` | `user_id` | `20260901120000_device_scoped_configs_drawings.sql` |

The code paths follow the same line. In `src/routes/sync/handler.rs` the todo and grocery futures
pass `&claims.sub` straight through to `process_*` and `fetch_remote_*` (lines ~53–201); the
config-and-drawing future starts with `let user_uuid = parse_or_hash_uuid(&claims.sub);` (line
~324) and passes the UUID from there down. `src/routes/devices/handlers.rs` derives the UUID in
all three handlers. `src/routes/sync/stream.rs` uses **both** in one request: the raw subject for
the Redis channel names (`sync_channel:{sub}`, via `publisher::get_channel_name`) and the derived
UUID for the config snapshot query. `src/routes/sync/status.rs` branches on scope — grocery/todo
scopes query on the raw subject, `ScribbleBox`/`ScribbleKeep`/`ScribbleKeepCloud` on the derived
one — while its Valkey cache key is keyed by the raw subject either way.

**Why this is the real risk here.** A future change that assumes a single identity introduces an
authorization bug, not a type error, because both values are "the user id" and both are just
strings on the wire. Two places already show the seam:

- `src/routes/user/deletion.rs` has to hold both at once — `delete_user_data(tx, user_id)` deletes
  todo and grocery rows by `user_id` and configs/drawings/devices by `parse_or_hash_uuid(user_id)`.
  Miss one and account deletion silently leaves a user's drawings behind.
- `src/jobs/reap_stale_users.rs` cannot express its join in SQL at all. `devices.user_id` is a
  one-way hash of `users.id`, so `find_stale_users` loads every `devices` row and every `users` row
  and joins them in Rust by hashing each subject. There is deliberately **no foreign key** between
  `devices` and `users`; the migration comment says so outright, because the column types genuinely
  do not line up.

There is also an asymmetry worth naming: the raw-subject side has a **sharing model** and the
derived side does not. Grocery rows are reachable through `grocery_list_members`, so a list can be
shared; configs, drawings and devices are single-account, scoped by `user_id` and `device_uuid`
only. Any feature that tries to share a drawing the way a grocery list is shared is crossing that
line and needs a design, not a query.

## 2. The derived UUID is publicly computable

`Uuid::new_v5(NAMESPACE_DNS, sub)` is a deterministic, **unkeyed** function of the subject. There
is no server secret in it. Anyone who knows a user's Google `sub` can compute that user's
`configs.user_id` / `drawings.user_id` / `devices.user_id` offline, in one line, with no access to
anything.

And the subject is not secret. This list said "two confirmed exposures" until 2026-09-06; the
count was wrong, and the correction is the point. **Six fields across five tables** hand the
subject to co-members, all in `src/routes/sync/grocery/remote_mutations.rs`, each query joining
through membership and returning the row's owning subject:

- `grocery_lists."ownerId"` (line 62) — the list owner's;
- `grocery_list_members."userId"` (line 98) — **every member's**;
- `stores."userId"` (line 135), `categories."userId"` (line 174),
  `grocery_items."userId"` (line 215), `grocery_item_store_info."userId"` (line 267) — whoever
  created the store, the category, the item, the price.

So sharing one grocery list discloses the raw subject of everyone who has ever touched it, not
just its members. A seventh channel is now closed: `join_handler` in
`src/routes/lists/handlers.rs` used to build membership row ids as
`format!("{}-member-{}", list_id, user_id)`, embedding the subject in an id that travelled with
the row; those ids are `gen_random_uuid()` since
[#78](https://github.com/toolateforteddy/teddy-fyi-api-rust/pull/78), though rows written before
it keep their old ids.

Closing the remaining six is `2026-09-05_pre_split_changes.md` item 46.

### What that does *not* enable today

Being able to compute the identifier is not being able to use it. Two things gate these paths, and
both were checked against the code:

1. **The auth middleware.** `src/auth/middleware.rs` requires an `HS256` JWT signed with
   `state.jwt_secret`, and additionally requires the `X-Client-UUID` header to equal the token's
   `client_uuid` claim. No token, no request.
2. **Every query scopes by the caller's own derived UUID.** The value comes from
   `parse_or_hash_uuid(&claims.sub)` — from the verified token — and never from the request body.
   `configs`/`drawings` reads and writes all carry `AND user_id = $n`
   (`src/routes/sync/config.rs`, `src/routes/sync/drawing.rs`), device handlers carry
   `AND user_id = $n` (`src/routes/devices/handlers.rs`), and the SSE snapshot is scoped the same
   way. `DrawingSyncItem` has a client-supplied `user_id` field, but it is **ignored on the way
   in**: the write path uses only the server-derived UUID.

So an attacker who knows your `sub` has your config UUID and nothing else. To use it they would
additionally need a token this service will accept for that subject — i.e. either a Google ID token
for your account, or the ability to forge one of our own JWTs, which means `JWT_SECRET`. With
either of those they would not need the UUID anyway; it would be derived for them.

The accurate statement is therefore: **the derived UUID is an identifier, not a capability.** The
defect is that it looks like an opaque random UUID and is not one. The risk is a future change
that treats it as unguessable — a lookup by `user_id` taken from a request body, a share link, an
"import from device X" flow, a debug endpoint — which would go from unexploitable to trivially
exploitable with no change to this function.

### Interaction with the `mock.` dev bypass

`src/auth/handlers.rs::login_handler` accepts any token beginning `mock.` when
`state.cookie_domain.is_empty()`, and in that branch takes `user_id` **from the request body**:

```rust
if payload.google_auth_token.starts_with("mock.") && state.cookie_domain.is_empty() {
    (payload.user_id.clone(), Some("dev-user@teddy.fyi".to_string()))
}
```

`COOKIE_DOMAIN` defaults to `.teddy.fyi` in `main.rs`, so the branch is off unless the variable is
explicitly set to the empty string — prod is not exposed today. But the bypass is what turns
"computable identifier" into "chosen identifier": a caller who reaches it names its own subject and
therefore lands on any config/drawing/device UUID it likes. **A sibling change is compile-gating
this branch out of release builds; that work is not part of this note and nothing here should be
read as a reason to touch `handlers.rs`.** It is recorded because the two findings compose, and
because if the gate ever comes off, section 2's "does not enable" paragraph stops being true.

### The `parse_str` branch, and whether it is reachable

If a subject *is* a valid UUID string it is used verbatim rather than hashed, so the two branches
share one output space and two different inputs can name the same identifier: the string
`"d35a2a2a-d1d1-55ed-90a7-348c3da59deb"` resolves to exactly the UUID that the subject `"user-1"`
hashes to.

**Not reachable with real Google `sub` values.** Google's `sub` is an opaque decimal digit string
(21 digits in current practice) — no hyphens, no hex letters, wrong length — so `Uuid::parse_str`
rejects every one of them and the hashing branch always wins. `Uuid::parse_str` is lenient about
*spelling* (hyphenated, unhyphenated, braced, any case) but not about alphabet or length, and a
digit-only string of that length parses as none of those forms.

It is reachable two other ways, both worth knowing:

- the `mock.` bypass above, where the caller picks the subject;
- the `client_id` and change-id call sites, where clients genuinely do send both UUID-shaped and
  non-UUID-shaped values, and where the collision is harmless because those are not identities.

Given that, it is a latent sharp edge rather than a live vulnerability: it becomes a real one the
moment any subject that is not a Google `sub` is introduced — a pairing-issued identity, a test
account, a migration-created user, a second IdP.

## 3. What a fix would cost

Two shapes of fix. Both have the same migration.

- **HMAC re-key.** `hmac_sha256(server_key, sub)` truncated to 128 bits, formatted as a UUID.
  Removes public computability; keeps the column types and every existing query. Introduces a key
  that must be present at boot, rotated never (or with a dual-read window), and backed up with the
  database — losing it orphans the data as thoroughly as changing the hash does.
- **One canonical identity.** Drop the derivation and key everything by the raw subject, changing
  `configs.user_id`, `drawings.user_id` and `devices.user_id` to `TEXT`. Strictly better long-term:
  it deletes the whole class of bug in section 1, restores a real FK to `users`, and lets
  `find_stale_users` become one SQL join. Larger diff, and it widens three indexed columns.

### What has to move

Three columns hold a derived **user** identity: `configs.user_id`, `drawings.user_id`,
`devices.user_id`. Two more hold a derived **client** value that would shift under an HMAC change
even though it is not identity: `configs.client_uuid`, `drawings.client_uuid`. And
`configs.id`/`drawings.id` are derived from client-supplied change ids for any client that sends a
non-UUID one — those are primary keys, referenced by clients' local databases, and **must not be
re-derived at all**; a re-key must therefore be applied to the user column only, not by re-running
`parse_or_hash_uuid` over everything.

Constraints and indexes that ride on the user column, all in the two migrations above:
`idx_configs_user_id`, `idx_drawings_user_id`, `idx_devices_user_id`, and
`unique_user_device_config_key UNIQUE (user_id, device_uuid, key)`.

### Ordering

The obstacle is that the old value cannot be inverted — `configs.user_id` does not tell you which
`sub` produced it. The mapping has to be reconstructed from `users.id`, hashing forward:

1. Add `user_id_v2` (nullable) to `configs`, `drawings`, `devices`. No behaviour change.
2. Backfill: for each `users.id`, set `user_id_v2` on rows where `user_id = old_derive(id)`. Then
   **count the rows left NULL** — those are rows whose subject is no longer in `users` (deleted
   accounts, or data predating the `users` table, which arrived a migration *after* configs and
   drawings did). They cannot be re-keyed and have to be a deliberate decision: delete, or park.
3. Deploy a **dual-write** build: writes fill both columns, reads still use `user_id`.
4. Backfill again to catch rows written between steps 2 and 3.
5. Deploy a build that reads `user_id_v2`.
6. Only then: drop `user_id`, rename, re-add the unique constraint and indexes.

Steps 3–5 are the ones that cannot be collapsed. Skipping the dual-write window is what loses the
mid-deploy rows.

### Rows written mid-deploy

Clients sync continuously and there is no maintenance window, so between the backfill and the
cutover the fleet is writing new configs and drawings under the **old** derivation. That is what
step 3's dual-write and step 4's second backfill exist for. Without them, every row written in that
gap becomes invisible after the cutover — and invisible in the worst way, because the client still
holds it locally and will re-upload it as an insert, producing duplicate rows under
`(user_id, device_uuid, key)` rather than an error anyone would notice.

The SSE stream needs the same care: `sync_channel:{sub}` is keyed by the raw subject and so is
unaffected, but `sync_channel:{sub}:device:{device_uuid}` carries a device id that a re-key does
**not** change, so streams stay coherent across the cutover. Worth confirming rather than assuming
if the canonical-identity option is taken instead.

### Rollback

Reversible up to and including step 5, because `user_id` is still populated and still authoritative
for the old build: roll the deployment back and the old column is intact. **Step 6 is the point of
no return** — once `user_id` is dropped, the old derivation cannot be recomputed from what remains
(you would need every `users.id`, and the un-backfillable rows from step 2 are gone for good). Leave
a long gap before step 6, and take a snapshot immediately before it.

## Notes for whoever picks this up

- **The characterisation tests in `src/routes/sync/tests/identity.rs` are meant to fail** when the
  derivation changes. If one of them goes red, that is the signal that the change in hand needs the
  migration above — not that the test is stale. Do not update the constants to match new output.
- The doc comment on `parse_or_hash_uuid` carries the short version, so the constraint is visible
  without finding this file. Keep the two in step.
- Nothing here is urgent on its own. What makes it urgent is a *new* feature keyed by the derived
  UUID, or any endpoint that accepts a user identifier from a request body. Either of those should
  come with this decision made, not deferred again.
