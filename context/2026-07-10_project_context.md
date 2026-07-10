# Project Context: teddy-fyi-api-rust (As of July 10, 2026)

This document serves as an entry-point context file for future developer agents working on the `teddy-fyi-api-rust` service.

---

## 🚀 Project Overview & Tech Stack
The backend is a **Rust-based Axum HTTP service** serving as the centralized source of truth and Sync Gatekeeper for `teddy.fyi`, a multi-tenant, local-first Android/iOS/Web collaborative ecosystem.

* **Core Framework**: Axum (with Tower middleware)
* **Database**: PostgreSQL managed via SQLx (compile-time checked queries, migrations under `/migrations`)
* **Caching**: Redis (caching sync states)
* **Authentication**: JWT (jsonwebtoken HS256) + Argon2 (for refresh token hashing)
* **Deployment**: Dockerized, running on Kubernetes (`kubectl get pods` deployment name: `api-rust-dep`)

---

## 📂 Key Codebase Directory Layout
Follows the modern Rust module layout (file-based submodules, no legacy `mod.rs` files):

* `src/main.rs`: Entry point. Initializes tracing (JSON format), database pool, Redis client, CORS, router nesting, and Axum listener.
* `src/auth.rs`: declarative entry point for the auth submodules.
* `src/auth/`
  * [handlers.rs](file:///Users/teddymartin/src/teddy-fyi-api-rust/src/auth/handlers.rs): HTTP routes for `/login` (Google OAuth token verification), `/logout`, and `/refresh` (session validation and token rotation).
  * [middleware.rs](file:///Users/teddymartin/src/teddy-fyi-api-rust/src/auth/middleware.rs): `require_auth` Axum middleware. Extracts `X-Client-UUID` and Bearer JWT, validates token signature (HS256), and attaches Claims to request extensions.
  * [tokens.rs](file:///Users/teddymartin/src/teddy-fyi-api-rust/src/auth/tokens.rs): Helpers for creating access tokens and hashing/verifying refresh tokens with Argon2.
  * [models.rs](file:///Users/teddymartin/src/teddy-fyi-api-rust/src/auth/models.rs): `Session` and token database struct mappings.
* `src/routes/`
  * `sync.rs` & `sync/`: Sync endpoint code (`/sync`, `/sync/status`), sub-scoped by features (todo, grocery).
  * `ai/`: Services interfacing with Gemini (for task icon allocation and grocery item categorizations).
  * `lists/`: Management of collaborative lists and invite flows.

---

## 🔐 Authentication & Session Lifecycle
* **Access Tokens**: Short-lived JWTs. Validated in the middleware via `HS256` signature verification. Requires matching `client_uuid` in the claims against the request's `X-Client-UUID` header.
* **Refresh Tokens**: Saved as hashes in the `sessions` DB table. Keyed on `(user_id, client_uuid)` primary key to support multi-device sessions.
* **Token Rotation & Grace Period**:
  * Every `/refresh` request rotates the refresh token (current token is stored, old token hash is saved to `old_refresh_token_hash`, and time is logged in `rotated_at`).
  * A **30-second grace period** allows reusing the old token to handle network drops and concurrent retries gracefully.
* **Concurrency Locking**:
  * To prevent parallel sync or refresh requests from corrupting the session row state, `refresh_handler` wraps verification and update inside a PostgreSQL transaction (`SELECT ... FOR UPDATE` row lock).
* **Mitigation Rules**:
  * **Session Expired**: If the refresh token is valid but expired, it is treated as a normal session termination. Only the expired device session is deleted.
  * **Invalid Token / Grace Period Exceeded**: If verification fails (e.g., mismatched token or old token reused after 30s), it is treated as a potential security breach. **All active sessions for that user are deleted**, requiring a full login on all their devices.

---

## 🔄 The Sync Endpoint Contract (`POST /api/sync`)
The core sync contract reconciles client states using an atomic endpoint.
* **Inbound Payload**:
  * `last_synced_at`: Timestamp (Unix millis) representing client's last sync checkpoint.
  * `client_id`: Unique client UUID to prevent echoing their own changes back to them.
  * Delta arrays: `todo_changes` and `grocery_changes` containing UUID `id`, change type (`INSERT/UPDATE/DELETE`), version counters, and updated state data.
* **Conflict Resolution**:
  * Uses a Last-Write-Wins (LWW) mechanism backed by version comparison.
  * Updates server tables and increments version counts.
* **Outbound Response**:
  * Status lists for uploaded adjustments.
  * Array of remote changes committed by other clients since `last_synced_at`.
  * `server_timestamp` anchor to serve as the client's next `last_synced_at`.
