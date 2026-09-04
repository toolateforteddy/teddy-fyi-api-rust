# Device pairing auth: signing in where there is no Google Play Services

Decided 2026-09-04. This is the API half of a three-repo change. The whole plan lives in
`ScribbleRoute-Labs/toybox` at `Context/FIRE_APPSTORE_PLAN.md`; **everything this repo has to
build is written out below**, so nothing here needs that file to be actionable. Read it only for
the product reasoning and for what the Android and website halves are doing.

Companion docs: [AGENTS.md](../AGENTS.md), [README.md](../README.md),
[2026-07-10_project_context.md](2026-07-10_project_context.md).

## Why

ScribbleKeep — the parent console that sits on the same tablet as the child app — is going to the
Amazon Appstore for Fire tablets. Fire OS has no Google Play Services, so the Android client's
`androidx.credentials` + `GetGoogleIdOption` sign-in cannot run there: there is no Google identity
provider on the device to answer it. The client never gets an ID token, so it never reaches
`POST /auth/login`, so the parent can never turn on backup.

The fix moves the Google half of sign-in to a device that *does* have a Google account. The tablet
asks us for a short code and polls; the parent signs in at `scribbleroute.com/link` on their phone
or laptop and types the code; we hand the tablet the same access/refresh pair `/auth/login`
already mints.

```
Fire ScribbleKeep                      this service                    scribbleroute.com/link
      │                                     │                                     │
      ├─ POST /auth/device/start ──────────>│                                     │
      │<── user_code, device_code ──────────┤                                     │
      │   shows "H4KP-9TQR"                 │                                     │
      │                                     │       parent signs in with Google ──┤
      │                                     │<── POST /auth/device/claim ─────────┤
      ├─ POST /auth/device/poll ───────────>│    (google id_token + user_code)    │
      │<── access_token, refresh_token ─────┤                                     │
```

RFC 8628 (OAuth device authorization grant) in shape, deliberately — but these are our endpoints
minting our own tokens. We are not implementing Google's device flow, and no Google endpoint is
called that `login_handler` does not already call.

**This is not Fire-specific in the end.** It is what any future TV, kiosk or shared-device install
wants, and it is a graceful fallback on any Android device whose Play Services are stripped or
broken.

## Module layout

Per `AGENTS.md` §4, `auth.rs` stays declarative. New code goes in `src/auth/device.rs` with its
unit tests in `src/auth/device/tests.rs`. Nothing lands in `auth.rs` but a `pub mod device;`.

---

## The work

- [ ] **1. Migration: `device_authorizations`.**
      `migrations/<timestamp>_create_device_authorizations.sql`.

      | Column | Type | Notes |
      | :-- | :-- | :-- |
      | `device_code_hash` | text, PK | Hashed with `auth::tokens::hash_refresh_token`, the same way refresh tokens already are — a database dump must not yield a usable code |
      | `user_code` | text, unique, indexed | What the parent types. Stored plain; it is short-lived and single-use |
      | `client_uuid` | text | Must match on poll, so a leaked device code is not portable to another install |
      | `user_id` | text, nullable | Null until claimed |
      | `attempts` | int, default 0 | Failed claims against this row |
      | `created_at` / `expires_at` | timestamptz | |
      | `claimed_at` / `consumed_at` | timestamptz, nullable | Claimed = a parent redeemed it; consumed = the tablet collected the tokens |

      Postgres, not Valkey. This is auth state and it should not evaporate with the cache — and
      unlike the sync-status cache, there is no cheap DB fallback to recompute it from.

- [ ] **2. Extract session minting from `login_handler`.** Steps 2–4 of
      `src/auth/handlers.rs::login_handler` — access token, 64-char random refresh token, `users`
      upsert, `sessions` upsert — become
      `issue_session(state, user_id, email, client_uuid, duration_secs) -> AuthResponse`.
      `login_handler` calls it, and so does the poll handler. **Pure refactor**: no behaviour
      change, and the existing `src/auth/tests.rs` must stay green without being edited.

- [ ] **3. `POST /auth/device/start`.** Unauthenticated, mounted on the public `auth_routes`
      router in `main.rs`.

      Request `{ client_uuid, app }`. Response:
      ```json
      { "device_code": "<64 alnum>", "user_code": "H4KP-9TQR",
        "verification_uri": "https://scribbleroute.com/link",
        "expires_in": 600, "interval": 5 }
      ```
      `device_code` is generated the way the refresh token in `login_handler` already is (64
      `Alphanumeric` samples). `user_code` is **8 characters from a 24-symbol unambiguous
      alphabet** — no `0`/`O`, no `1`/`I`/`L`, and no vowels, so a code can never spell a word a
      parent has to read aloud with embarrassment. Displayed `XXXX-XXXX`. Retry generation on
      collision with an unexpired row.

- [ ] **4. `POST /auth/device/claim`.** Request `{ google_auth_token, user_code }`.

      Verify the Google ID token *exactly* as `login_handler` does —
      `state.google_client.validate_id_token`, then the `state.google_client_ids` audience check —
      then upsert the `users` row and stamp `user_id` + `claimed_at` on the matching unexpired,
      unclaimed row. `204` on success.

      On an unknown, expired or already-claimed code: `404`, and increment a per-user failure
      counter. **Five failures in ten minutes locks that Google account out of claiming.** Eight
      characters from 24 symbols is a large space and a code lives ten minutes, but guessing must
      still cost something.

      Never log a `user_code` or a `device_code`, at any level.

- [ ] **5. `POST /auth/device/poll`.** Request `{ device_code, client_uuid }`.

      | Condition | Response |
      | :-- | :-- |
      | Unclaimed, unexpired | `202` `{"status":"pending"}` |
      | Claimed | `200` + `AuthResponse`, stamping `consumed_at` **in the same transaction** so a code is single-use |
      | Expired, or already consumed | `410` |
      | Polled faster than `interval` | `429` |
      | `client_uuid` does not match `/start` | `404` — same shape as an unknown code, so it is not an oracle |

- [ ] **6. CORS — this one will bite.** `main.rs` currently allows exactly one origin:
      ```rust
      .allow_origin("https://teddy.fyi".parse::<HeaderValue>().unwrap())
      ```
      and `.layer(cors)` sits outside `.nest("/auth", auth_routes)`, so it governs the new
      endpoints too. `/auth/device/claim` is called from a browser at **`scribbleroute.com`**,
      which that layer blocks today. It needs a second allowed origin — as configuration, a list,
      not `Any`. `allow_credentials(true)` is already set, which makes a wildcard origin invalid
      anyway.

- [ ] **7. Reaping.** Expired rows are dead weight and mild risk. Either extend the
      `src/jobs/reap_stale_users.rs` pattern with a sweep, or delete on the `expires_at` index —
      whichever sits better in the job runner already there.

- [ ] **8. Verify the audience env — check this first, it is five minutes.**
      `login_handler` accepts an ID token only if its `aud` is in `state.google_client_ids`, loaded
      by `src/auth/client_ids.rs` from `GOOGLE_CLIENT_IDS`, `GOOGLE_CLIENT_ID` and
      `SCRIBBLEROUTE_API_CLIENT_ID`. The website will present a token whose `aud` is the
      ScribbleRoute **web** client, `34718544535-n0eabvd30ebmn4npqnpq7dlgmt9qn3pe.apps.googleusercontent.com`
      (`AuthConfig.SERVER_CLIENT_ID` in the toybox repo).

      Confirm it is in the deployed environment. If it is not, `/claim` returns `401` for every
      parent, and the only trace is the `tracing::warn!` audience-mismatch line inside
      `login_handler`'s equivalent — invisible from the client, which sees a bare 401.

- [ ] **9. Tests** in `src/auth/device/tests.rs`: happy path; wrong code; expired code; replayed
      device code; poll before claim; `client_uuid` mismatch; rate limit; audience mismatch.

---

## Notes for whoever picks this up

- **Nothing here changes an existing endpoint's behaviour.** Step 2 is the only edit to existing
  code, and it is a pure extraction. If a test in `src/auth/tests.rs` has to change, the
  extraction is wrong.
- **The tablet is the child's device.** The point of this design is that the parent's Google
  credentials are never typed on it. Any shortcut that puts a Google flow back on the tablet
  defeats the reason the endpoints exist.
- **Two base URLs are in play and it is not clear which is canonical.** The Android client's
  `DEFAULT_BASE_URL` says `api.scribbleroute.com`; the toybox auth spec and the website's `env.ts`
  default say `api-rust.teddy.fyi`. Presumably one service behind two names — but the `/link` page
  must call whichever one CORS is configured for in step 6, so settle it before writing that
  config rather than after.
