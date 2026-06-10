# AI Context: Sync Backend Engine & API Contract (Phase 2)

## Project Overview
This Rust service (Axum/Actix-web) acts as the centralized Sync Gatekeeper and source of truth for a multi-tenant, local-first Android and iOS ecosystem. It must manage low-concurrency, collaborative data streams (e.g., household shared grocery lists and private user to-do lists).

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

## Expectations for Gemini/Rust Assistant
1. Prioritize strict type safety, transaction isolation, and explicit error handling for database writes.
2. Ensure the sync engine avoids the "echo" problem by accurately utilizing the `client_id` filter.
3. Keep the payload formats perfectly mirrored to the Android client schema requirements.
