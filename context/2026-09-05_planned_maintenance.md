# Planned maintenance for ScribbleRoute: the runbook

*Written 2026-09-05, alongside `context/2026-09-05_scribbleroute_backend_split.md`. Status:
**built and applied-ready, never yet used.** Nothing in this note is switched on.*

Phase 4 of the split freezes ScribbleRoute writes while the data tier is copied and re-keyed.
That freeze is a real outage, and ScribbleKeep is on tablets belonging to people who are not the
person running the migration. This note is how they find out.

The mechanism is three pieces in three repos, and they are deliberately independent — each one is
useful on its own and none of them can silently fail open into announcing an outage that is not
happening.

| Piece | Repo | What it does |
|---|---|---|
| `k8s/maintenance.yaml` | here | An nginx that answers 503 with an explanation on `api.scribbleroute.com`. |
| `site-ingress` | `teddyfyi` | Points the hostname at that nginx instead of `api-rust-svc`. |
| `public/status.json` + `/status` | `website` | The document the apps read to tell *planned* apart from *broken*. |

The apps are the fourth piece, in `toybox`: they fetch the status document after a sync fails,
and only then. ScribbleKeep does not fetch it at all unless cloud sync is switched on — an app
that has never made a network request has no business asking whether the network is down.

## Why a separate nginx and not a flag on the API

The obvious implementation is `MAINTENANCE_MODE=1` and a middleware in `src/`. It is one line and
it is wrong for this window specifically: during Phase 4 the API is either scaled to zero or
pointed at a database it must not serve from, so the process that would be carrying the flag is
the process that cannot run. A maintenance responder that is down during maintenance explains
nothing.

The one detail that is easy to get wrong is in the ConfigMap and commented there: **the health
check must not be 503.** The GKE ingress only routes to backends its health check calls healthy,
so an nginx that 503s the probe as well gets marked unhealthy and clients receive Google's own
502 page instead of the body — which turns a deliberate, explained outage back into an
unexplained one. `/healthz` answers 200; everything else answers 503.

## Opening a window

1. **Ahead of time, at leisure.** Apply `k8s/maintenance.yaml`. It takes no traffic until step 3,
   so this is safe to do days early, and doing it early is the point: it is the step that can
   fail, and you do not want to discover an image pull problem at the top of the window.
   Confirm with `kubectl get pods -l app=scribbleroute-maintenance` and a direct port-forward:

   ```
   kubectl port-forward deploy/scribbleroute-maintenance-dep 8080:8080
   curl -i localhost:8080/healthz      # 200
   curl -i localhost:8080/api/sync     # 503, Retry-After: 1800, JSON body
   ```

2. **Fill in the window.** In the `website` repo, edit `public/status.json`: the `notice` copy and
   the `startsAt` / `endsAt` times. Leave `state` at `operational`. The copy is inert until the
   flag flips, so this can be reviewed and merged well in advance. See
   `website/context/STATUS_PAGE.md`.

3. **At the top of the window, in this order:**
   1. Flip `"state"` to `"maintenance"` in `status.json` and roll out the site. **First**, so a
      tablet that fails a sync one second later already has something true to read.
   2. Point `api.scribbleroute.com` at `scribbleroute-maintenance-svc` in
      `teddyfyi/k8s/ingress.yaml` and apply. The GCE load balancer takes a minute or two to
      re-target; until it does, requests still reach the live API, which is harmless.
   3. Only now do the destructive part — scale `api-rust-dep` down, or swap `DATABASE_URL`.

   The order matters in one direction only: announcing before breaking is always safe, and
   breaking before announcing is the window in which a parent sees an unexplained failure.

4. **Closing.** Exactly the reverse. Bring the API back and confirm it is serving, point the
   ingress back at `api-rust-svc`, wait for the load balancer, *then* set `status.json` back to
   `operational`. Clearing the announcement last means the apps never tell a parent everything is
   fine while the hostname is still 503ing.

   Leave the `notice` copy in `status.json` where it is. It is ignored while the state is
   operational, and having last time's wording to edit beats writing it again under pressure.

5. **Rollback, at any point:** point the ingress back at `api-rust-svc`. That is the whole
   rollback — the maintenance Deployment holds no state and can be left running or deleted at
   leisure.

## What clients do with the 503

`Retry-After: 1800` is the only header here with a behavioural effect. The Android client parses
it (`core/data-cloud`, `RetryAfter`) and holds that sync scope off for the stated time, clamped
to ten minutes at the client. That clamp is the intended outcome rather than a limitation: long
enough to stop a fleet of tablets hammering a hostname that is deliberately down, short enough
that nothing is stranded if the window closes early.

The JSON body names `statusUrl` and `statusDocumentUrl`. Nothing depends on a client reading
them — the apps go to `https://scribbleroute.com/status.json` from a compiled-in constant, not
from whatever a 503 body tells them to fetch — but they are there for anyone reading the response
by hand, and for a future client that has no constant of its own.

## When the fork happens

All of this is ScribbleRoute's and moves with it. That is why every name in
`k8s/maintenance.yaml` is qualified `scribbleroute-maintenance-*` rather than bare like the
resources in `api-rust.yaml`: the split note's warning about a fork inheriting unqualified names
and overwriting production applies to any file added here between now and then, and the cheapest
time to get a name right is before it exists.
