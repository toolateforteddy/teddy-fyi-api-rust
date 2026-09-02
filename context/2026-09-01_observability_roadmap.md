# Observability Roadmap: analytics, monitoring, downtime detection

Decided 2026-09-01. Companion docs: [2026-08-27_dev_environment_roadmap.md](2026-08-27_dev_environment_roadmap.md),
[AGENTS.md](../AGENTS.md), [README.md](../README.md).

**Decisions made up front, so the phases below are not re-litigating them:**

| Question | Decision |
| :-- | :-- |
| Metrics + dashboards | **GCP-native.** Google Managed Prometheus scrapes `/metrics`; dashboards and alert policies in Cloud Monitoring. Logs already land in Cloud Logging. |
| Alert delivery | **Custom Android app over FCM**, with the **official Google Cloud mobile app** enabled underneath it as the always-works fallback. |
| Product analytics | **Log-based metrics only.** No new storage, no vendor, no `events` table. |
| Kubernetes manifests | **Separate `teddyfyi` infra repo.** Nothing k8s-shaped lands in this repo; cross-repo access to be granted when needed. |

---

## 0. Where we actually are

Not "bad instrumentation" — mostly *absent* instrumentation, plus one component
that reports health it does not measure.

* **The liveness probe cannot fail.** `/healthcheck` in `main.rs` is
  `get(|| async { "OK" })` — a static string with no dependency check. The real
  deep check, `readiness_handler` (pings Postgres), is mounted at `/api/ready`
  **inside the `require_auth` layer**, so a kubelet probe gets a 400 for the
  missing `X-Client-UUID` and can never reach it. A pod whose Neon connection is
  dead therefore stays in the Service endpoints indefinitely, serving 500s. K8s
  will not restart it and will not take it out of rotation.
* **No request-level telemetry.** `tower-http` is compiled with the `cors`
  feature only — no `TraceLayer`, no request id, no latency, no status codes.
  The 102 `tracing::{error,warn,info}!` calls are all hand-placed. Any 500 on a
  path where nobody typed an error log is completely invisible.
* **No metrics.** Zero `prometheus` / `opentelemetry` / `metrics` references in
  `src/` or `Cargo.toml`. "Is it slow?" and "how many 5xx this hour?" are
  currently unanswerable.
* **Redis degradation is silent by construction.** Every call site is
  `if let Ok(conn) = ...get_multiplexed_tokio_connection()`, failures logged at
  `warn` and swallowed; `/api/sync/status` falls through to the expensive DB
  aggregate. Redis dying is a performance cliff with no alarm attached.
* **Deploys are not verified.** `deploy.yml` ends at
  `kubectl rollout restart deployment/api-rust-dep` with no `rollout status`. A
  crashlooping image produces a green workflow.
* **Nothing checks the service from outside the cluster.** DNS, TLS cert, and
  ingress failures are undetectable by anything running in the cluster.
* **No product analytics.** No DAU, no sync volume, no Gemini call or cost count.

What *does* already work, and is under-used: `tracing_subscriber::fmt().json()`
means every log line is structured JSON on stdout, so GKE is already shipping
them into Cloud Logging, indexed and queryable. `kubectl logs` is the weak way
to read something that is already in a real log store.

---

> **Status, 2026-09-01: phases 0 and 1 are implemented and verified locally.**
> Phases 2–4 are console and app work and remain open. Two things in this
> document changed during implementation and the text below reflects the change,
> not the original plan: readiness does **not** check Postgres (cost constraint,
> §1), and `scope` moved off the `http_request` line onto its own event (§2).

## 1. Phase 0 — stop the health check from lying

Highest value per hour of work in the whole document. Converts a class of silent
outage into an automatic pod restart.

**In this repo:**

* `GET /healthz/live` — process is up. Static `OK` is correct *here*: liveness
  failure means "kill the pod", and a shared-dependency outage must not restart
  every replica simultaneously. Verified to stay `200` with Redis unreachable.
* `GET /healthz/ready` — **pings Redis only.** Returns 503 with a JSON body
  naming the failed dependency (`{"status":"unready","failed":"redis"}`),
  bounded by a 2s timeout so a hung connection reports unready instead of
  hanging the kubelet.

  > **Postgres is deliberately excluded.** Neon scales compute to zero and bills
  > per wake-up, so a probe running `SELECT 1` on a timer would keep the database
  > awake around the clock purely to answer the probe — on a low-traffic service
  > that is the dominant cost, spent on monitoring rather than users. The
  > handler takes a `redis::Client` rather than `AppState`, so the constraint is
  > enforced by the type signature and not by a comment.
  >
  > Postgres health is recovered **passively** instead — see §1a. The
  > authenticated `/api/ready` keeps the deep database check for on-demand use.

* `/healthcheck` and `/api/ready` are retained for now, so this deploy cannot
  strand a rollout whose probes still point at the old paths. Delete both once
  the cluster is repointed.

### 1a. Passive Postgres health

`src/observability/db_health.rs`. Readiness cannot *probe* Postgres without
paying a wake-up per probe, but it can watch the errors real requests already
produce. Every `?` on a `sqlx::Error` converts through one
`impl From<sqlx::Error> for AppError`, so a single hook there sees every database
failure in the service, for zero extra queries.

Three failure classes, three meanings:

| Class | Meaning | Counts toward unreadiness? |
| :-- | :-- | :-- |
| **Answered** (constraint violation, `RowNotFound`, decode) | The server replied — proof of connectivity | No — it **clears** the streak |
| **Unreachable** (I/O, TLS, closed pool, or `PoolTimedOut` on a pool that cannot fill) | Nothing answered | Yes |
| **Saturated** (`PoolTimedOut` on a full pool) | Load, not outage | No — metric only |

Three failures within 120s marks the replica unready with
`{"status":"unready","failed":"postgres"}`. Both thresholds are env-tunable
(`DB_UNHEALTHY_AFTER_FAILURES`, `DB_UNHEALTHY_WINDOW_MS`).

**Four things end-to-end testing against a genuinely stopped Postgres corrected,
each of which had silently defeated the detector:**

1. **An unreachable database does not produce `sqlx::Error::Io`.** The pool
   absorbs the failed connects and retries internally until `acquire_timeout`,
   so the caller sees `PoolTimedOut` — the *same variant as real load*. The
   first implementation classified on the error alone and therefore filed every
   real outage under "saturated", the one class that never trips readiness.
2. **The two are told apart by whether the pool is full**, not by whether it is
   empty. `size()` counts connections *being established*, so it is non-zero
   during a failing acquire; it only drains to 0 *after* the acquire gives up.
   `size() < max_connections` reads correctly in both directions.
3. **The window must outlast `acquire_timeout`.** Each failing request burns a
   full 30s acquire timeout, so serial retries land ~30s apart. With a 30s
   window each failure aged out the previous one, the streak never passed 1, and
   the flag could not fire. Hence 120s.
4. **Recovery needs the pool's idle count.** Successful queries never reach the
   error funnel, so nothing clears the streak on recovery — a replica served
   503s for the remainder of the window while already returning 200s to users.
   An idle connection is proof the database is answering, so `is_degraded`
   clears on it.

Each of the four is pinned by a named regression test.

**Why saturation is excluded**: `max_connections` is 5. If load could make every
replica report unready at once, the load balancer would be left with no endpoints
and a slowdown would become a total outage. Being busy is not a reason to shed
traffic.

**Residual limits, stated honestly.** An outage while ≥5 requests are in flight
can momentarily look like a full pool and read as saturation; that direction is
the safe one and defers detection to the error-rate alert. A replica receiving no
traffic reports ready — correct, since it has no evidence of failure and has not
spent a wake-up looking for one. And detection costs up to three failed requests
of latency (~90s at the default `acquire_timeout`), which is the real argument
for lowering that timeout — see §7.

**In `teddyfyi`:**

* `readinessProbe` → `/healthz/ready`, `livenessProbe` → `/healthz/live`,
  plus a `startupProbe` so slow migration-on-boot does not trip liveness.
  Note `init_postgres()` runs `sqlx::migrate!` on every boot, so first-request
  readiness genuinely lags process start.
* Confirm the Deployment has resource requests/limits — without them there are
  no meaningful utilization alerts and no HPA later.

**In `deploy.yml`:**

```yaml
- name: Deploy to GKE
  run: |
    kubectl rollout restart deployment/api-rust-dep
    kubectl rollout status deployment/api-rust-dep --timeout=5m
```

A failed rollout should turn the workflow red. Right now it cannot.

*Estimate: half a day, $0.*

---

## 2. Phase 1 — make the service emit signal

Everything downstream (dashboards, alerts, **and analytics**) is derived from
this phase. Since analytics is log-based, the field names chosen here are
load-bearing and effectively permanent — log-based metrics cannot be backfilled.

### 2a. Request logging

`Cargo.toml`: `tower-http = { version = "0.6.10", features = ["cors", "trace", "request-id", "util"] }`
and add `env-filter` to `tracing-subscriber`.

One structured line per completed request, emitted from a `TraceLayer`
`on_response` hook, with a **stable, deliberately chosen** field set:

| Field | Why |
| :-- | :-- |
| `event: "http_request"` | The filter predicate every log-based metric keys on. |
| `method`, `route` | `route` = the *matched* axum path (`/api/sync`), never the raw URI — otherwise cardinality explodes on `/api/devices/:id`. |
| `status` | 5xx rate, 4xx rate. |
| `latency_ms` | Distribution metrics for p50/p95/p99. |
| `request_id` | Correlates the error log with the request log. |
| `client_uuid` | Per-device debugging; also the DAU denominator. |
| `user_hash` | See the privacy note below. |
| — | `scope` is **not** here; see below. |

Probe traffic to `/healthz/*` is excluded from both the log line and the
metrics: it would otherwise drown the request log, inflate every log-based
metric with traffic no user generated, and add nothing, since a failing
readiness check already logs its own warning with the reason attached.

**`scope` moved to its own event.** It lives in the request body, which
middleware cannot read without buffering the whole payload. Rather than pay that
on every request, `POST /api/sync` emits a second line —
`event="sync_completed"` with `scope`, `uploaded`, and `downloaded` — plus
matching counters. This is strictly better for analytics than the original plan:
"how many entities moved, in which scope" is the question worth asking of a sync
backend, and request count alone cannot answer it.

Also switch to `EnvFilter` so `RUST_LOG` works, and default to `info`.

> **Privacy note, worth settling before writing the field.** Logging a raw
> `user_id` puts a copy of user-identifying data into Cloud Logging that
> `jobs::reap_stale_users` and `DELETE /api/user/data` do not erase — which sits
> awkwardly next to the retention commitment published at
> scribbleroute.com/privacy. Two defensible options: (a) log a salted hash
> (`user_hash`), keeping per-user *counting* while making the log non-erasable
> data non-identifying, or (b) log the raw id and shorten the log bucket
> retention so the log store's own retention *is* the erasure guarantee. Option
> (a) is the recommendation and is what the table above assumes.

### 2b. Metrics endpoint

`axum-prometheus` (or `metrics` + `metrics-exporter-prometheus`), exposed on a
**second listener on port 9090**, not through the main router. Managed Prometheus
scrapes the pod IP directly, so `/metrics` never needs to be reachable through
the ingress and never needs an auth exemption.

Metrics worth having on day one:

* RED per route — request count by `route`/`status`, latency histogram.
* `db_pool_connections` / `db_pool_idle` against `max_connections(5)`, which is
  low and a plausible bottleneck under SSE load. Also the evidence behind the
  outage-vs-load call in §1a, so it is worth seeing directly.
* `db_connectivity_degraded` (0/1) and `db_connectivity_failures_total` by class.
* **`redis_degraded_total`** — incremented at every `if let Ok(conn)` fallback
  site. This is the whole point: it converts the deliberate silent fallback into
  something a dashboard can show and an alert can fire on.
* Gemini call count, latency, and outcome (`ok` / `http_error` /
  `transport_error`) — the only spend-shaped metric in the service.
* SSE: currently-connected `/api/sync/stream` clients, held by an RAII guard
  captured in the stream itself, so it decrements when the client disconnects —
  the only moment a disconnect is actually observable.

Metrics that may legitimately never fire (`redis_degraded_total`,
`sse_connections_active`) are pre-registered at zero at boot. An exporter only
renders a series once something records to it, so without that a *healthy*
service reports "no data" — indistinguishable from a broken exporter or an
un-deployed build, which is precisely the ambiguity monitoring exists to remove.

### Verified locally

`make test` 103 passed (97 before, 6 added). `cargo clippy -- -D warnings` — the
CI gate — clean; `--all-targets` still 30 warnings, all pre-existing, none in new
code. `.sqlx` untouched, so the offline build and the deploy are unaffected.

Against a running server: `x-request-id` generated and client-supplied values
both propagate to the response and into the log; the matched path is logged
(`route: "unmatched"` on a 404, never the raw URI); `user_hash` appears only on
authenticated requests; `/healthz/*` emits no request log lines. With Redis
pointed at a dead port: readiness 503 naming `redis`, liveness still 200,
`/api/sync/status` still 200 via the database fallback, and
`redis_degraded_total` incrementing on exactly the two sites that failed.

Against a real stopped Postgres (an isolated throwaway container, so the normal
dev containers were untouched): healthy → three failed syncs → readiness 503
`failed: postgres` with `db_connectivity_degraded 1` → database restarted → the
very next sync 200 and readiness back to 200 in the same breath.

---

## 3. Phase 2 — GCP-native collection

**In `teddyfyi`:** a `PodMonitoring` custom resource pointing Managed Prometheus
at port 9090 / `/metrics`. Managed Prometheus is enabled by default on recent
GKE Autopilot and is a cluster-level toggle on Standard — verify which `prod`
(us-central1-a) is before assuming.

**In Cloud Monitoring:** one dashboard. Request rate, 5xx rate, p50/p95/p99
latency, pod restarts, `redis_degraded_total`, sqlx pool saturation, SSE
connections, Gemini calls/errors.

**Log-based metrics** defined off `jsonPayload.event="http_request"` — this is
also the analytics substrate:

* `sync_success_count` — `route="/api/sync"` AND `status=200`. Feeds the
  heartbeat alert in Phase 3, which is the single most valuable alert here.
* `daily_active_clients` — distinct `client_uuid`.
* `sync_by_scope` — labeled by `scope`.
* `auth_failures` — `route` under `/auth/*` AND `status>=400`.

> **Retention caveat.** Log-based *metrics* are retained as time series like any
> other metric (on the order of 24 months), so the counters above are safe
> long-term. Raw logs in the `_Default` bucket expire at 30 days, so ad-hoc
> queries ("what did user X do in March") have a 30-day horizon. If a longer
> window is ever wanted, the answer is a log sink to BigQuery — but that is
> explicitly deferred, and it re-opens the privacy question above.

*Estimate: 1 day.*

---

## 4. Phase 3 — detection and alerting

**Detection must not run inside the thing that can be down.** This rules out a
sidecar in the same cluster and rules out the Android app polling the API.
Google's globally distributed uptime probers do the detection; the cluster being
on fire does not impair them.

**Uptime check** → `https://<public host>/healthz/live` from multiple regions.
This is the leg that catches DNS, TLS expiry, and ingress failures, none of
which any in-cluster signal can see.

**Alert policies**, roughly in order of how much they are worth:

1. **No successful sync in N minutes** — metric-absence / threshold on
   `sync_success_count`. For a sync backend this is the best single alert there
   is: it catches "broken in a way that still returns 200 to a health check,"
   which no HTTP probe can. Pick N against real observed traffic troughs so
   overnight quiet does not page; start generous (60m) and tighten.
2. **Uptime check failing** from ≥2 regions.
3. **5xx ratio** above threshold over a rolling window.
4. **Pod restart / crashloop rate** — catches a bad deploy that `rollout status`
   somehow let through.
5. **p99 latency** regression on `/api/sync`.
6. **`redis_degraded_total` rising** — warning severity, not a page.

**Notification channels, layered deliberately:**

* **Google Cloud mobile app** — wired first, on every policy. Zero code, works
  today, and is the fallback that keeps working on the day the custom app has a
  bug. This is the safety net, not the plan.
* **Custom Android app** — the actual project:

  ```
  Alert policy → Pub/Sub notification channel → Cloud Function → FCM → app
  ```

  The Cloud Function translates the Monitoring notification JSON into an FCM
  message. The app is then a thin FCM receiver plus a glanceable status screen.
  Deliberately: detection is Google's, the push channel and the UI are yours, and
  nothing in the chain runs in the cluster being monitored. The one part that
  needs care is alert *resolution* — Monitoring sends a `closed` notification,
  and the app should collapse open/closed into a single updating notification
  rather than two unrelated pings.

  Build it after Phase 3's policies are firing correctly into the Cloud app, so
  there is a known-good reference for what the notifications look like.

*Estimate: 1 day for the GCP side; a weekend for the app.*

---

## 5. Sequencing

| | Work | Cost | Repo |
| :-- | :-- | :-- | :-- |
| **0** | ✅ Health endpoints, passive DB health, `rollout status` done; **probes still to apply in `teddyfyi`** | — | here + `teddyfyi` |
| **1** | ✅ Request logging, `EnvFilter`, `/metrics` on :9090 | — | here |
| **2** | `PodMonitoring`, dashboard, log-based metrics | 1 day | `teddyfyi` + GCP console |
| **3** | Uptime check, alert policies, Cloud app channel | 1 day | GCP console |
| **4** | Pub/Sub → Cloud Function → FCM → Android app | a weekend | new |

Phases 0 and 1 are the ones that matter; 2 and 3 are mostly console work that is
only possible *because* of 1. Phase 4 is the fun part and is strictly additive —
if it never ships, the Cloud mobile app still pages you.

**Explicitly out of scope**, so it does not creep in: an `events` table,
BigQuery export, PostHog or any third-party analytics, OpenTelemetry distributed
tracing (one service, no fan-out to trace), and self-hosted Prometheus/Grafana.

**Operational note.** GCP console and `gcloud`/`kubectl` work against `prod`
needs the working gcloud binary in `~/Downloads`, not the one on `PATH`.

---

## 6. Handoff to `teddyfyi`

Nothing below is applied yet — the code is deployed-ready, the cluster is not
repointed. Until this lands, the new endpoints exist but nothing probes them.

On `deployment/api-rust-dep`, alongside the existing container port:

```yaml
ports:
  - { name: http,    containerPort: 8080 }
  - { name: metrics, containerPort: 9090 }
livenessProbe:
  httpGet: { path: /healthz/live, port: http }
  periodSeconds: 10
  failureThreshold: 3
readinessProbe:
  httpGet: { path: /healthz/ready, port: http }
  periodSeconds: 10
  timeoutSeconds: 3        # > the handler's own 2s Redis bound
  failureThreshold: 3
startupProbe:
  httpGet: { path: /healthz/live, port: http }
  periodSeconds: 5
  failureThreshold: 30     # `init_postgres()` migrates on every boot
```

`startupProbe` matters more here than it looks: `init_postgres()` runs
`sqlx::migrate!` before the listener binds, and on a cold Neon compute that is
a slow start that would otherwise trip liveness into a restart loop.

Then a `PodMonitoring` pointing Managed Prometheus at the `metrics` port and
`/metrics`. The metrics port is intentionally *not* exposed through the ingress:
Managed Prometheus scrapes the pod IP directly, so `/metrics` never needs an
exemption carved out of `require_auth`.

**Optional environment variables** (all have working defaults):
`METRICS_PORT` (9090), `RUST_LOG` (`info`), `LOG_HASH_SALT` (falls back to
`JWT_SECRET`), `DB_UNHEALTHY_AFTER_FAILURES` (3), `DB_UNHEALTHY_WINDOW_MS`
(120000). Note that leaving `LOG_HASH_SALT` unset means rotating
`JWT_SECRET` also rotates every `user_hash`, breaking per-user continuity across
that rotation — set it explicitly if that continuity matters.

Console and `kubectl` work against `prod` needs the working gcloud binary in
`~/Downloads`, not the one on `PATH`.

---

## 7. Open recommendation: lower the pool `acquire_timeout`

Not changed, because it is a behaviour change beyond observability and the Neon
interaction is a judgement call — but §1a made the cost concrete.

`PgPoolOptions` in `main.rs` sets `max_connections(5)` and leaves
`acquire_timeout` at the sqlx default of **30 seconds**. Measured against a
stopped database, every request hung for the full 30s before returning 500. Two
consequences: users wait 30s for an error, and the passive detector needs ~90s of
serial failures to trip.

Something like `acquire_timeout(Duration::from_secs(10))` would make failures
fast and detection roughly three times quicker. The reason it is a judgement call
and not an obvious win: Neon compute resume from scale-to-zero costs seconds on
the first request after an idle period, and that resume happens *inside* the
acquire. Too short a timeout turns a cold start into a user-visible error. 10s
leaves generous headroom over a typical resume while cutting the outage case to a
third — but it should be chosen against observed cold-start latency, which is
exactly what the new `http_request_duration_seconds` histogram will show after a
week in production. Worth revisiting then rather than guessing now.

If it changes, `DB_UNHEALTHY_WINDOW_MS` should stay at roughly 4× whatever the
acquire timeout becomes, for the reason in §1a item 3.
