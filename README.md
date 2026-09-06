# teddy.fyi Sync API & Backend Service

This is the centralized, high-performance Sync Gatekeeper and source of truth backend service for the **teddy.fyi** ecosystem. Built with Rust, it manages multi-tenant, collaborative, local-first data streams (e.g., shared household grocery lists and private todo lists) for both iOS and Android clients.

> Working in this repo rather than calling it? Start with [`CLAUDE.md`](CLAUDE.md):
> the module map, how to build, test and validate a change here, and the five hard
> constraints. This file is the API contract and the setup guide.

---

## 🏗 System Architecture & Tech Stack

```mermaid
graph TD
    Client[Mobile Clients iOS/Android] -->|HTTPS / JWT| API[Axum / Tokio API Gateway]
    API -->|Read/Write / Transactions| Postgres[(Neon Serverless PostgreSQL)]
    API -->|Sync Status Cache| Redis[(Valkey / Redis Cache)]
    API -->|AI Categorization / Emojis| Gemini[Gemini 2.5 Flash Lite]
```

- **Web Framework**: [Axum](https://github.com/tokio-rs/axum) with [Tokio](https://tokio.rs/) for high-concurrency async performance.
- **Database**: [PostgreSQL (Neon)](https://neon.tech/) with [SQLx](https://github.com/launchbadge/sqlx) for type-safe queries and database migrations.
- **Cache**: [Valkey / Redis](https://valkey.io/) to cache sync states, enabling fast, low-overhead sync check-ins.
- **AI Integrations**: [Gemini API](https://ai.google.dev/) (specifically `gemini-2.5-flash-lite`) for automated grocery item categorization and todo list emoji/icon generation.
- **Auth**: Google OAuth & JWT (JSON Web Tokens) with secure cookie-based session management.

---

## ⚡ Core Features

### 1. Atomic Sync Protocol (`POST /api/sync`)
Exposes a single endpoint to reconcile state changes between the local database on devices (SQLite/Room) and the server's Postgres database:
- **Permission Check**: Validates that the requesting `user_id` has access to the target list (e.g., shared lists require list membership).
- **Conflict Detection (MVCC & LWW)**: 
  - Compares incoming `client.version` against `server.version`.
  - Increments version numbers upon matching version edits.
  - Resolves conflicts using Last-Write-Wins (LWW) if versions mismatch, forcing clients to align with the bumped server version.
- **Echo Prevention**: Filters out database updates that originated from the requesting client (`client_id`) to save bandwidth.

### 2. Fast Sync Check (`GET /api/sync/status`)
A lightweight endpoint that client apps hit on startup to determine if a full sync is necessary:
- Uses a Redis/Valkey cache containing user-scoped `last_update` timestamps.
- Falls back to database aggregate queries on cache misses.

### 3. AI-Powered Smart Helpers
- **Item Auto-Categorization (`POST /api/categorize`)**: Automatically maps new grocery item titles (e.g., *"organic whole milk"*) to a user's customized categories (or standard fallback categories like *"Dairy"*).
- **Todo Emojis (`POST /api/assign-icon`)**: Inspects todo titles and returns a highly relevant emoji or icon token to display next to the task.

---

## 📁 Module Layout & Development Standards

This repository strictly adheres to modern Rust codebase standards.

> [!IMPORTANT]
> **No `mod.rs` files**: We follow the modern Rust file-based module layout. Sibling submodules of `routes.rs` are defined in a sibling `routes/` folder. All module entry files (e.g. `routes.rs`, `auth.rs`, `sync.rs`) are declarative and contain only `pub mod` and `pub use` statements. No handler logic or unit tests belong in these file-based entrypoints.

---

## 🛠 Setup & Local Development

### Prerequisites
- [Rust toolchain](https://rustup.rs/) (managed automatically via `make init`)
- [Docker](https://www.docker.com/) (for running local Redis/Valkey)
- [NodeJS / npm](https://nodejs.org/) (for installing and running `neonctl`)

### 1. Environment Configuration
Create a `.env` file from the template:
```bash
cp .env.example .env
```
Ensure you have the following environment variables configured:
* `DATABASE_URL`: Connection string to your database.
* `JWT_SECRET`: Secret key used for signing JWTs.
* `GEMINI_API_KEY`: API key for accessing Gemini services.
* `GOOGLE_IOS_CLIENT_IDS`: Comma-separated client IDs of the iOS apps. Google's iOS sign-in flow issues ID tokens whose `aud` is the app's own client ID, so each of these is also accepted as an audience.
* `GOOGLE_CLIENT_IDS`: Comma-separated client IDs for everything that is not an iOS app.

  Between them these two form the accepted-audience allowlist, and at least one **must** be set in a real deployment — a normal build now refuses to start with the allowlist empty, because it could never authenticate anybody and used to say so only in a single log line at boot. The legacy single-value vars `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_ID_GROCERY_WEB`, and `SCRIBBLEROUTE_API_CLIENT_ID` are still honored and count towards it. Local dev omits all of them and signs in with `mock.` tokens instead; see [The `dev-auth` feature](#the-dev-auth-feature) below.

  Device pairing has no bypass of its own: `POST /auth/device/claim` always validates the ID token, so the ScribbleRoute **web** client ID must be in this allowlist or every parent's claim returns `401`. It is currently supplied by `SCRIBBLEROUTE_API_CLIENT_ID`.
* `CORS_ALLOWED_ORIGINS` (optional): Comma-separated browser origins allowed to call this API. Defaults to `https://teddy.fyi,https://scribbleroute.com,https://www.scribbleroute.com`. Never a wildcard — `allow_credentials` is on, which makes one invalid anyway.
* `DEVICE_VERIFICATION_URI_<APP>` (optional): Where a tablet running `<APP>` tells the parent to go to redeem a pairing code, e.g. `DEVICE_VERIFICATION_URI_TEDDY_FYI`. `<APP>` is the `app` the client sends to `/auth/device/start`, uppercased with every non-alphanumeric character written as `_`. This service is shared, and the two products redeem codes on two different websites, so with nothing set each known app falls back to its own page — `SCRIBBLE_KEEP` and `SCRIBBLE_BOX` to `https://scribbleroute.com/link`, `TEDDY_FYI` and `TEDDY_FYI_GROCERY` to `https://teddy.fyi/link`. Set one of these only to point a single app at a staging site.
* `AUTH_RATE_LIMIT_BURST` / `AUTH_RATE_LIMIT_REPLENISH_MS` (optional): Per-IP rate limit across all of `/auth/*`. Defaults to a burst of `30` with one request returned every `500` ms. Sized for a household behind one NAT address with several tablets polling `/auth/device/poll`; raise it only if that stops being true.
* `DEVICE_START_RATE_LIMIT_BURST` / `DEVICE_START_RATE_LIMIT_REPLENISH_MS` (optional): The tighter bucket on `/auth/device/start` alone, which stacks on top of the general one. Defaults to a burst of `5` with one request returned every `15000` ms — that endpoint runs an Argon2id hash (~19 MiB, tens of ms) before it has authenticated anything, so it is the one that turns a flood into an outage. These four exist so an incident can be ridden out with a rollout restart instead of a rebuild; a missing, zero or unparseable value falls back to the default rather than disabling the limiter. Note the limits key on `X-Forwarded-For`, which means they trust the ingress to set it.
* `DEVICE_VERIFICATION_URI` (optional): The redemption page for a caller that named no app, or an app this build does not know. Defaults to `https://scribbleroute.com/link`. It does **not** override the per-app pages above.
* `COOKIE_DOMAIN` (optional): The `Domain` attribute put on the `access_token` cookie. Defaults to `.teddy.fyi`. Empty is a legitimate value and means *no* `Domain` attribute — a host-only cookie, which is what a single-host deployment wants. It affects the cookie and nothing else. It used to double as the gate on the development login bypass, so choosing the empty value silently turned on impersonation of any account; that coupling is gone, and the bypass is now a compile-time feature (below).

### The `dev-auth` feature

`POST /auth/login` normally validates a Google ID token. There is one bypass, for local development: a token beginning `mock.` is accepted without verification and mints a session for whatever `user_id` the request body names. That is total impersonation, so it is gated at **compile time** by the `dev-auth` cargo feature and is simply not present in any build that does not name the feature — including `cargo build --release`, which is what the `Dockerfile` runs. No environment variable, header or request can re-enable it in a shipped binary.

What this means in practice:

| | `dev-auth` build (`make dev`) | Normal build (`make run`, release, Docker) |
| --- | --- | --- |
| `mock.` tokens | accepted, logged at `warn` | rejected like any other invalid token |
| Google client IDs configured | **must be none** — the process refuses to start otherwise | **at least one required** — the process refuses to start otherwise |

Both start-up assertions are deliberate belt-and-braces (`src/auth/dev_bypass.rs`). A dev-auth binary that also carries real Google client IDs is what a development build escaping onto a real deployment looks like, and a normal binary with no client IDs can never authenticate anybody — better a failed rollout than a running service where every login 401s.

**Running locally.** `make dev` (via `scripts/dev.sh`) enables the feature for you and is the supported path. If you are not using that script:

```bash
cargo run --features dev-auth              # or: make run-dev-auth
cargo watch -x 'run --features dev-auth'   # hot reload
```

Leave `GOOGLE_CLIENT_IDS`, `GOOGLE_IOS_CLIENT_IDS` and the three legacy single-value vars unset in your `.env` when you do. If you specifically want to exercise *real* Google sign-in locally, drop the feature instead and configure a client ID — that is then the same binary shape production runs.

**Testing.** `make test` runs the production feature set, which is what CI runs and what proves a shipped binary rejects `mock.` tokens. `make test-dev-auth` runs the other half; a few tests exist only in one configuration or the other, so a change to `src/auth/dev_bypass.rs` wants both.

### 2. Run with Automated Dev Script
The easiest way to start development is to run:
```bash
make dev
```
The underlying development script ([scripts/dev.sh](file:///Users/teddymartin/src/teddy-fyi-api-rust/scripts/dev.sh)) handles the following tasks:
1. Retrieves your active Neon project ID using `neonctl`.
2. Cleanly deletes any expired, orphaned developer branches.
3. Automatically provisions a temporary database branch (`dev-<username>-<timestamp>`) cloned from your production branch.
4. Generates connection strings configured with statement caching options tuned for PgBouncer compatibility (`statement_cache_size=0`).
5. Configures your local `.env` file with the connection string.
6. Boots up a local Redis/Valkey Docker container (`teddy-redis-dev`) if it's not already running.
7. Automatically applies all database migrations in `/migrations`.
8. Starts the Axum server with `--features dev-auth` (see [The `dev-auth` feature](#the-dev-auth-feature)) using `cargo watch` for hot-reloading (falls back to `cargo run` if not installed).

---

## 📋 Makefile Commands Reference

Run these commands from the project root:

| Command | Action |
| :--- | :--- |
| `make init` | Installs the Rust toolchain |
| `make install` | Fetches Cargo dependencies |
| `make build` | Compiles the release/debug binary locally |
| `make run` | Starts the server locally |
| `make dev` | Spins up the branch databases, Redis container, and runs hot-reloaded dev server |
| `make test` | Executes the unit/integration test suite |
| `make prepare` | Prepares SQLx offline metadata cache for offline compiler verification |
| `make docker-build` | Builds the Docker production image |
| `make docker-run` | Runs the API container on port `8080` |
| `make clean` | Cleans up the target directory |

> **If you add, remove or edit a `sqlx::query!` — run `make prepare` and commit `.sqlx/`.**
> CI compiles with `SQLX_OFFLINE=true`, so the query macros are checked against the
> committed `.sqlx/` descriptors rather than a database; a descriptor that no longer
> matches the schema would otherwise compile green and fail against real Postgres. The
> "Check SQLx Offline Cache" step in CI regenerates the descriptors against a live
> database with the migrations applied and fails if they differ from what is committed.
> `make prepare` needs `DATABASE_URL` pointing at a database with every migration run.

---

## 🔌 API Endpoint Documentation

### Authentication
All routes under `/api` require Google OAuth or JWT authorization.
- `POST /auth/login` - Exchanges a Google OAuth token for access/refresh tokens.
- `POST /auth/refresh` - Renews an expired access token.
- `POST /auth/logout` - Revokes current session cookies.

##### `user_uuid` on the login and refresh answers

`/auth/login` and `/auth/refresh` both return an extra field alongside the tokens:

```json
{ "access_token": "...", "refresh_token": "...",
  "user_uuid": "6f1b0c2e-...-9d41" }
```

`user_uuid` is `users.surrogate_id` — an opaque UUID that encodes nothing about the account.
The browser-shaped answers (`?client=web`) carry it too, next to the existing `user_id`.

It is deliberately **not** called `user_id`. That name is taken twice over and means the raw
Google subject in both places: `BrowserAuthResponse.user_id` returns the subject, and
`RefreshRequest.user_id` is the subject a client sends back. Returning a different kind of
identifier under the same name is how a client ends up comparing two things that were never
the same and concluding the wrong thing.

**What a client should do with it.** Store it, and compare it against whatever it currently
holds as its user id — today that is a subject a client base64-decoded out of a JWT. *A
mismatch is the signal to migrate the local database*, whose rows are keyed by the old
identifier. Shipping the field now, before any sync payload changes, is what gives clients a
release in which to learn that check; see item 46 of
`context/2026-09-05_pre_split_changes.md`.

The field is **omitted** when the column is NULL, so a response without it parses exactly as
today's does. In practice it is always present: the column has a `gen_random_uuid()` default
and migration `20260911120000` backfilled every existing row.

`POST /auth/device/poll` returns the same body `/auth/login` does, so a paired tablet gets
`user_uuid` at the moment it collects its session rather than waiting for its first refresh.

#### Device pairing (no Google Play Services)

A Fire tablet has no Google identity provider on the device, so it can never produce the ID
token `/auth/login` needs. These three unauthenticated endpoints move the Google half of
sign-in to a browser on a device that does have an account. RFC 8628 in shape, but they are
our endpoints minting our own tokens.

- `POST /auth/device/start` - The tablet asks for a code.
  - **Request**: `{ "client_uuid": "...", "app": "scribblekeep" }`
  - **Response (`200 OK`)**:
    ```json
    { "device_code": "<64 alnum>", "user_code": "H4KP-9TQR",
      "verification_uri": "https://scribbleroute.com/link",
      "expires_in": 600, "interval": 5 }
    ```
  - The tablet displays `user_code` and keeps `device_code` to itself.
- `POST /auth/device/claim` - The parent redeems the code from `scribbleroute.com/link`.
  - **Request**: `{ "google_auth_token": "<Google ID token>", "user_code": "H4KP-9TQR" }`
  - `204` on success. `404` for anything unknown, expired or already claimed — deliberately
    one response, so it cannot be used to sort real codes from invented ones. `429` once a
    Google account has failed five claims in ten minutes.
- `POST /auth/device/poll` - The tablet collects its session.
  - **Request**: `{ "device_code": "...", "client_uuid": "..." }`

    | Condition | Response |
    | :-- | :-- |
    | Unclaimed, unexpired | `202` `{"status":"pending"}` |
    | Claimed | `200` + the same `{ access_token, refresh_token }` `/auth/login` returns |
    | Expired, or already consumed | `410` |
    | Polled faster than `interval` | `429` |
    | `client_uuid` does not match `/start` | `404` |

Expired and spent rows are swept by the `reap-stale-users` job, which runs the device sweep
before the account sweep.

### Sync Endpoints
#### `GET /api/sync/status`
Checks if the client needs to fetch new updates from the server.
- **Query Parameters**:
  - `last_synced_at` (optional): RFC 3339 formatted timestamp (e.g., `2026-06-18T18:34:46Z`).
  - `scope` (optional): `ALL`, `GROCERY`, or `TODO`.
- **Response (`200 OK`)**:
  ```json
  {
    "needs_sync": true,
    "latest_version": "2026-06-18T18:35:00Z"
  }
  ```

#### `POST /api/sync`
Main synchronization payload reconciling local changes with remote updates.

`last_synced_at` is the cursor. Store the `server_timestamp` from each reply and send it
back here; the server then returns only what changed after it. Omitting it asks for
everything the account owns.

`supports_paging` (optional, default `false`) says the client will come back for the rest of
a download the server cut short. When it is set, the download is bounded at
`SYNC_DOWNLOAD_PAGE_SIZE` rows per entity, the reply carries `has_more: true`, and its
`server_timestamp` is walked back to the last whole millisecond the reply delivered — so the
ordinary "store it and send it back" loop resumes exactly where the page ended. When it is
absent the reply is unbounded, because a page a client cannot ask past would cost it those
rows rather than defer them.

- **Request Body**:
  ```json
  {
    "last_synced_at": "2026-06-18T18:00:00Z",
    "client_id": "client-uuid-here",
    "scope": "ALL",
    "supports_paging": true,
    "todoListChanges": [],
    "todoChanges": [
      {
        "id": "todo-task-uuid",
        "type": "UPDATE",
        "version": 2,
        "data": {
          "id": "todo-task-uuid",
          "title": "Buy groceries",
          "isCompleted": true,
          "createdAt": 1718000000000,
          "position": 1,
          "scheduledAt": 1718000000000,
          "priority": 0,
          "sync_state": "SYNCED",
          "version": 2,
          "is_deleted": false
        }
      }
    ],
    "groceryListChanges": [],
    "groceryListMemberChanges": [],
    "storeChanges": [],
    "categoryChanges": [],
    "groceryChanges": [],
    "groceryItemStoreInfoChanges": []
  }
  ```
- **Response (`200 OK`)**:
  ```json
  {
    "success_ids": ["todo-task-uuid"],
    "upload_status": [
      {
        "id": "todo-task-uuid",
        "version": 3,
        "sync_state": "SYNCED"
      }
    ],
    "remote_todo_list_changes": [],
    "remote_todo_changes": [],
    "remote_grocery_list_changes": [],
    "remote_grocery_list_member_changes": [],
    "remote_store_changes": [],
    "remote_category_changes": [],
    "remote_grocery_changes": [],
    "remote_grocery_item_store_info_changes": [],
    "server_timestamp": "2026-06-18T18:35:00Z"
  }
  ```

### AI Helper Endpoints
#### `POST /api/categorize`
Categorize a grocery item name into specific user categories.
- **Request Body**:
  ```json
  {
    "item_title": "organic whole milk"
  }
  ```
- **Response (`200 OK`)**:
  ```json
  {
    "category": "Dairy"
  }
  ```

#### `POST /api/assign-icon`
Generates an appropriate icon/emoji for a todo item title.
- **Request Body**:
  ```json
  {
    "todo_title": "Schedule dental appointment"
  }
  ```
- **Response (`200 OK`)**:
  ```json
  {
    "emoji_or_asset_token": "🦷"
  }
  ```
