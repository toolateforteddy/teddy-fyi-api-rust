# Cloud sync as an entitlement: only ScribbleKeep Cloud creates accounts

Decided 2026-09-06. This is the API half of a two-repo change. The client half lives in
`ScribbleRoute-Labs/toybox` at `Context/CLOUD_ENTITLEMENT.md`; **everything this repo has to build
is written out below**, so nothing here needs that file to be actionable.

Companion docs: [2026-09-04_device_pairing_auth.md](2026-09-04_device_pairing_auth.md),
[2026-09-05_scribbleroute_backend_split.md](2026-09-05_scribbleroute_backend_split.md),
[2026-09-05_user_identity_derivation.md](2026-09-05_user_identity_derivation.md).

## The decision

Cloud sync and backup are a privilege, not a property of installing an app. Having acquired and
installed **ScribbleKeep Cloud** is what unlocks them.

Expressed as one rule about this service:

> **Only ScribbleKeep Cloud may create an account. Every other ScribbleRoute client may only sign
> in to one that already exists.**

A parent who installs ScribbleKeep on the tablet and has never used Cloud cannot sign in, so
cannot turn on sync, so stays local-only — which is a complete, working product, not a
degraded one. A parent who has set up Cloud on their phone signs in on the tablet exactly as
today.

## What "an account" is

The `users` row. Nothing more.

```sql
CREATE TABLE IF NOT EXISTS "users" (
    "id" TEXT PRIMARY KEY,     -- the raw Google auth subject
    "email" TEXT,
    "created_at" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_at" TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
```

There is **no entitlement table and no migration**. This was considered and rejected: a separate
table would need creating, backfilling, adding to `delete_user_data`, adding to the retention
reaper, and keeping consistent with `users` forever — to record a fact that the presence of the
`users` row already records exactly as well.

Three consequences come free, and each is one we would otherwise have had to build:

* **Nothing to backfill.** Every parent syncing ScribbleKeep today already has a `users` row,
  created by their own earlier sign-ins. Switching those clients to select-only signs *nobody*
  out. There is no staged tolerate-then-require rollout of the kind `src/auth/product.rs`
  documents, because there is no window in which a legitimate existing session is refused.
* **Revocation already works.** `delete_user_data` (`src/routes/user/deletion.rs:207`) and
  `jobs::reap_stale_users` both `DELETE FROM users`. Deleting the row de-entitles the tablet at
  its next sign-in, with no new code and no second thing to remember to delete.
* **Erasure stays honest.** There is no new copy of anything about a user, so nothing new for
  `DELETE /api/user/data` to miss.

## Where accounts are created today

Two `INSERT INTO users`, and the first is not where it looks:

| Site | Reached from | Whose Google audience is known there |
| :-- | :-- | :-- |
| `src/auth/handlers.rs:151`, inside `issue_session` | `login_handler` (`handlers.rs:247`) **and** the pairing poll (`device.rs:652`) | login: the app's own. poll: **none** |
| `src/auth/device.rs:436`, inside `claim_for_user` | `POST /auth/device/claim` | the parent's, from the browser or app that redeemed the code |

`issue_session` is shared, and the pairing poll is the reason it cannot simply be gated in place:
by the time a tablet polls, it presents a `device_code` and nothing else. Its own request carries
no proof of anything — that is the entire design of the pairing flow, and `device.rs:407` says so.
There is no audience there to judge.

So creation moves out of `issue_session` and up to the two places a Google audience actually
exists:

* **`issue_session` never inserts.** It updates `email` on an existing row and mints the tokens.
  Its callers guarantee the row exists before calling it.
* **`login_handler`** decides from the client ID it just validated the ID token against.
* **`claim_for_user`** decides from the redeeming parent's client ID.
* **The poll** needs no check at all. Whatever the claim decided has already been decided; a
  tablet cannot reach a session the claim refused to create.

That is a better shape than the one this change started with, independent of entitlement:
account creation now happens only where the identity of the creating *client* is known, rather
than in a helper two call paths deep that cannot see it.

## The rule is a denylist, not an allowlist

`may_create_account` defaults to **true**, and names the clients that may not.

This matters and the opposite polarity is a live outage. `users` is shared with teddy.fyi, so the
same `INSERT` is how a new grocery or todo user signs up. `GOOGLE_IOS_CLIENT_IDS` is deliberately
a mixed, unclassified bag spanning both products (`src/auth/client_ids.rs`), and
`GOOGLE_CLIENT_ID`/`GOOGLE_CLIENT_IDS` are unclassified too. An allowlist of creators would refuse
account creation to every client ID nobody had got round to classifying — including teddy.fyi's
iOS signup.

So the configuration is small and auditable: ScribbleKeep's client IDs, and nothing else.
ScribbleBox needs no entry — it does not take `:core-data-cloud` at all, holds no account, and
never calls this service. The `SyncScope::ScribbleBox` data is written by ScribbleKeep on the same
tablet.

This is the reverse of how `Product` classification works, and deliberately so. `Product`
restricts what an *already authenticated* caller may reach, so unknown-means-permitted is the
conservative reading and the boundary hardens as IDs are classified. Here unknown-means-permitted
is conservative for the same reason but at a different cost: the thing being refused is a signup
for a product that is not ours to gate.

## What this does to pairing

`POST /auth/device/claim` currently creates accounts, and the comment above the insert
(`device.rs:433`) states the assumption plainly:

> The parent may not have an account yet — the tablet has never signed in — so the `users` row is
> created here rather than at poll time.

Under this decision that assumption no longer holds: a parent who has no account is a parent who
has not set up Cloud, and pairing is not the place to fix that. Pairing puts a tablet **into an
account that already exists**. Creating the account is Cloud's job.

So the `/link` page on the website needs no special case and no separate entitlement check — it
falls under the same rule as every other non-Cloud client, and a claim from a parent with no
account is refused.

**The claim's refusal may be distinguishable.** `/auth/device/claim` returns one `404` for
anything unknown, expired or already claimed, so that a caller cannot sort real codes from
invented ones. "This Google account has no ScribbleRoute account" is not information about the
code — it is information about the credential the caller already holds and has proved they hold —
so it can have its own response without weakening that property. Answer it `403`.

The same reasoning covers `/auth/login`. Refusing with a clear, distinguishable reason tells the
caller something about a Google account they have just proved they control, which is not a
disclosure: anyone who can pass `validate_id_token` for that subject can already sign into it. The
ScribbleKeep client has to be able to say *why* it cannot sync, or every parent who installs the
tablet app first files a bug. The existing per-IP buckets on `/auth/*` (`src/rate_limit`) are the
only rate limiting this needs.

Per hard constraint 5, the refusal logs a hashed identifier — never the subject, never the email.

### Pairing from inside ScribbleKeep Cloud

Not required by this change, and worth doing anyway. `claim_handler` is unauthenticated and takes
a Google ID token plus a `user_code`; nothing about it needs a browser. ScribbleKeep Cloud already
holds an ID token, so it can call `/auth/device/claim` directly and skip the URL and the second
sign-in.

It also strengthens what `device.rs:407` already relies on. That comment observes that the
redeeming audience is the one thing in the handshake proving which product a tablet is being
paired into. When the audience is Cloud's own client ID, that proof extends to the entitlement:
possessing a Cloud ID token *is* the entitlement, so in-app pairing is self-gating.

Requires only that Cloud's Android client ID be present in `SCRIBBLEROUTE_CLIENT_IDS` — a
configuration change with no deploy. The website `/link` page stays, unchanged, for parents who
have an account and cannot use the app.

## Why now, and not after the split

This is a `2026-09-05_pre_split_changes.md`-shaped item: cheap now, expensive later. Four reasons,
in ascending order of how much they cost to defer.

**1. It needs no migration, so the Phase 2–3 window does not apply.** Hard constraint 3 and split
plan §1.2 forbid new migrations during Phases 2 and 3. This change adds none. But that is only
true while "an account" means the `users` row — and see (3).

**2. After Phase 4 the rule has to be written twice, or once and forgotten.** Post-fork the
ScribbleRoute binary owns login and pairing, and the teddy.fyi binary owns its own signup. The
denylist above exists precisely because those two share one `INSERT`. Writing it now means the
fork inherits a rule that has been running in production; writing it later means writing it into a
codebase that has never enforced it, during the phase whose whole point is that it is a pure
refactor.

**3. Phase 5 re-keys `users` and this check is defined against `users`.** Decision 8 of the split
plan makes `users.id` an opaque surrogate UUID with the Google subject demoted to an attribute
(`2026-09-05_identity_model.md`). "Does a row exist for this subject?" stops being a primary-key
lookup and becomes a lookup through the new identity structure. Landing the rule first means Phase
5 migrates a check that already exists and has tests; landing it after means designing it against
an identity model that is itself new.

**4. The cross-product leak this accepts is dissolved by the split, not deepened by it.** Because
`users` is shared, a teddy.fyi grocery or todo user has a `users` row, and therefore satisfies the
entitlement check. This is knowingly accepted: grocery today is one household — the author's own —
and is not exposed publicly before the split. Split plan decision 2 then removes the overlap by
construction:

> A person who uses both products becomes two accounts. Same Google identity, two `users` rows in
> two databases, two `sessions` rows, no shared state.

After Phase 5 a ScribbleRoute `users` row can only have been created by a ScribbleRoute client,
and — under this rule — only by Cloud. The check tightens on its own with no further work. That is
the opposite of a shortcut accruing interest, and it is the reason not to spend a table on
partitioning something the split is about to partition anyway.

Note also what §1.2 says about the alternative: `users.id` is the raw subject while
`configs.user_id`, `drawings.user_id` and `devices.user_id` are `parse_or_hash_uuid(sub)`, one-way
for non-UUID subjects, so **there is no SQL query that selects "the `users` rows belonging to
ScribbleRoute accounts."** Any attempt to scope the entitlement in the database today would need
the Rust-side join `find_stale_users` already works around. Accepting the shared table avoids that
entirely.

## Deliberately not done

* **`/auth/refresh` does not check.** Only `/auth/login` and `/auth/device/claim` do. A live
  session therefore survives revocation for up to its seven days. This is wanted during rollout
  and is the honest trade: sessions are the thing whose sudden death shows up to a parent as being
  signed out for no reason, and immediate revocation is not a requirement today. Revisit if it
  becomes one.
* **No client-side enforcement is trusted.** ScribbleKeep hiding the sync toggle is UX — it stops
  a parent walking into a dead end. A repackaged APK skips it. Every check that matters is here.
* **Nothing changes for ScribbleBox.** It has no account, no network layer, and per toybox hard
  constraint 2 gains no data capture from this.

## The change

1. `src/auth/client_ids.rs` — a `may_create_account(aud) -> bool` on `ClientCatalog`, defaulting
   true, reading a comma-separated denylist env var. Start-up logging lists what it holds, as
   `unclassified` already does for `Product`.
2. `src/auth/handlers.rs` — `issue_session` stops inserting into `users`; it updates `email` on
   the existing row. Document that callers guarantee the row.
3. `src/auth/handlers.rs` — `login_handler` creates or refuses, from the validated audience,
   before calling `issue_session`. `403` on refusal.
4. `src/auth/device.rs` — `claim_for_user` does the same from the redeeming parent's audience.
   `403`, distinct from the deliberate `404`.
5. Tests, in **both** feature configurations (`make test` and `make test-dev-auth`): a denied
   client with no row is refused; a denied client with an existing row succeeds; a permitted
   client creates; pairing claim refused for a denied audience with no row; poll after a
   successful claim still mints; the deny path logs no subject or email
   (`src/routes/sync/tests/log_hygiene.rs` is the pattern).
6. `README.md` — the two new `403`s and the new environment variable.

No `sqlx::query!` gains or loses a column, but the `users` insert becomes an update, so
`make prepare` and a committed `.sqlx/` are required.
