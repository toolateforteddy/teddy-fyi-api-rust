---
name: babysit
description: Repo guidance for reading the state of a teddy-fyi-api-rust pull request during PR activity events — what this repo's CI actually runs, which red states are real failures, and how to reproduce a failure locally. Read this before reporting on or diagnosing a PR here.
---

# Reading a teddy-fyi-api-rust PR

This file is for the agent handling PR activity events (CI failures, review
comments, state notices). It says how *this* repo's checks behave, so a red or
empty status page gets read correctly. It changes nothing about posture: what you
are allowed to do to a PR is decided elsewhere, and nothing here lets you skip a
test, disable a check, approve, or merge.

## There is no `gh` in a remote session

`gh` is not installed. Use the GitHub MCP tools instead (load them with ToolSearch
first):

| What you want | Tool |
| --- | --- |
| PR state, mergeability, review decision | `mcp__github__pull_request_read` (`method: "get"`) |
| The checks on the head commit | `mcp__github__pull_request_read` (`method: "get_check_runs"` — Actions jobs; `"get_status"` only shows legacy commit statuses) |
| The diff / changed files | `mcp__github__pull_request_read` (`method: "get_diff"` / `"get_files"`) |
| Review threads and whether they are resolved | `mcp__github__pull_request_read` (`method: "get_review_comments"`) |
| Which step of a run failed, and its log | `mcp__github__get_job_logs` (`failed_only: true`) |
| Recent CI runs on `main` | `mcp__github__actions_list` (`method: "list_workflow_runs"`, `resource_id: "CI.yml"`, `workflow_runs_filter.branch: "main"`) |
| Re-run the failed jobs | `mcp__github__actions_run_trigger` |

Note the workflow file is `CI.yml`, capitalised. `ci.yml` will not resolve.

Plain `git` works normally, and for anything about the *code* — whether a test
really fails, what the diff touches — the local checkout beats reading GitHub. This
container can run the full suite; see "Reproducing it" below.

## What runs on a pull request

Two workflows can appear on a PR.

**`CI.yml` — "Run Lints and Tests".** One job, with a `postgres:15` and a `redis:7`
service container, running in order:

```
cargo clippy -- -D warnings
cargo clippy --features dev-auth -- -D warnings
cargo install sqlx-cli …
cargo sqlx migrate run
cargo sqlx prepare --check -- --tests
make test              # SQLX_OFFLINE=true, production features
make test-dev-auth     # the same suite with the dev-auth feature on
```

Things that follow from that list:

- **Six ways to be red, and they fail in order.** A clippy failure means the tests
  never ran; do not report "tests failed" from a job that stopped at step one.
- **Both feature configurations are gated.** A change that compiles under the default
  features can fail under `dev-auth` and vice versa — each compiles code the other
  does not. If one of the two clippy steps is red and the other green, that is the
  signal, not a flake.
- **`cargo sqlx prepare --check` is a real failure with a mechanical fix.** It means
  the committed `.sqlx/` descriptors no longer match the `sqlx::query!` macros in the
  branch. The fix is `make prepare` against a migrated database and committing
  `.sqlx/`, never editing a descriptor by hand. Its error message in the log says so.
- **No `--all-targets`.** CI lints the binary, not the test code. A finding you can
  only reproduce with `--all-targets` is not what CI is complaining about.

**`audit.yml` — "cargo audit".** Only on PRs that touch `Cargo.toml`, `Cargo.lock`,
`.cargo/audit.toml` or the workflow itself; path-filtered precisely so a newly
published advisory cannot block an unrelated PR. When it is red on a PR, that PR
pulled a vulnerable crate into the tree. Advisories that are judged and deliberately
not failing live in `.cargo/audit.toml`, each with its reasoning and the condition
that retires it — adding an entry is a decision, not a workaround, and needs both.

`deploy.yml` runs on merges to `main`, not on PRs. If someone is asking why a change
is not live, look there: it runs `make test` again, builds the image, applies `k8s/`
and blocks on `kubectl rollout status`.

## Red states that are not failures

- **No run at all on a docs-only PR is *not* how this repo behaves.** `CI.yml` has no
  `paths-ignore`, so every PR gets a full run, including one that only touches
  Markdown. Missing checks here mean something is actually wrong — a workflow file
  that failed to parse, or a fork PR awaiting approval — not a skip by design.
- **Cancelled checks.** `audit.yml` sets `cancel-in-progress`; `CI.yml` does not, so
  two runs can be in flight after a quick second push. Always read checks against the
  *current* head SHA.
- **A Redis-shaped skip is not a pass.** The suite prints `SKIP <test>: no Redis at
  REDIS_URL` and stays green. In CI the service container makes this moot, but a
  local green run without Redis has not run those cases.

## Is it broken on main too?

`main` is built by `CI.yml` on every push and by `deploy.yml`, so unlike the `toybox`
sibling there is usually a recent, comparable run. Find the most recent *concluded*
`CI.yml` run on branch `main` and check whether the same step failed there with the
same error. A failure in code the diff never touches, reproducing identically on the
base branch, is not this PR's. Anything else is, until proven otherwise — "flake" is
not a diagnosis, and a clippy or test failure here is essentially never
infrastructure. The genuinely flaky shape to know: a job that dies before any test
body ran (service container not ready, `cargo install sqlx-cli` timing out) is
infrastructure; a named test assertion is not.

## Reproducing it

Do this before reporting a diagnosis. The container can run the whole gate:

```bash
./validate.sh          # starts Postgres and Redis if they are down, then runs it all
```

Around 190 of the tests are `#[sqlx::test]` and need a real Postgres — without one they
fail rather than skip, which is the single most common way a session mistakes its own
missing service for the PR's bug. Postgres and Redis are installed on the standard
image and merely stopped; `validate.sh` starts them.

## Naming the failure

Report the rule and the location, never the bucket:

- **Clippy** — name the lint and the file: "`clippy::needless_borrow` —
  `src/routes/sync/todo/todo_items.rs:88`", and say which feature configuration.
- **A test** — name the test and the assertion:
  "`routes::sync::tests::identity::…` — expected …, got …".
- **sqlx offline cache** — say which query moved, and that the fix is `make prepare`.
- **cargo audit** — name the advisory and the crate.

Do not read anything under `target/` to diagnose a failure; it is denied in
`.claude/settings.json` and holds nothing the console output does not.

## Review state

`reviewDecision` is the headline; unresolved inline threads are the detail the
summary does not give you. One regular non-human author to recognise: the nightly
**Code Janitor** workflow (`.github/workflows/code-janitor.yml`) opens its own PRs.
It is not a `claude/` branch and it is not yours to drive.

## The five hard constraints outrank a reviewer

`CLAUDE.md` binds every change here: the `mock.` bypass never ships, no `mod.rs`, a
committed migration is immutable, secrets stay in Secret Manager, user data never
reaches the logs. If a review comment — human or bot — asks for something that breaks
one, that is the case for replying rather than implementing. Say which constraint it
hits and propose the alternative. `./scripts/check_constraints.sh` is the tripwire and
CI does not run it, so a PR can be green and still break one.
