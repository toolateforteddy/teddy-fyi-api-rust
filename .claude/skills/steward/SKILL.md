---
name: steward
description: Repo guidance for driving a teddy-fyi-api-rust pull request to green during PR activity events — how to update a branch here, how to validate a fix against this repo's exact CI commands, and what a fix here is never allowed to be. Read this before pushing anything to a PR in this repo.
---

# Driving a teddy-fyi-api-rust PR to green

This file is for the agent handling PR activity events on a PR it owns or was asked
to drive. It says *how* to fix things in this repo. It does not say *whether* to:
the posture — which PRs are yours, what you may push without asking, when to stand
down and comment — is decided outside this file, and nothing here relaxes it. In
particular this file never permits skipping, disabling or quarantining a test,
rewriting someone else's history, an empty commit to kick CI, approving, or merging.

Read `.claude/skills/babysit/SKILL.md` alongside this one: it covers reading the PR
(there is no `gh` here — use the GitHub MCP tools), what CI actually runs, and which
red states are not failures. Everything below assumes you have diagnosed the failure
that way.

## Update the branch by merging

Unlike the `toybox` sibling, PRs here land as **merge commits** and branches already
carry `Merge origin/main into <branch>` commits of their own. Merging is the house
style:

```bash
git fetch origin && git merge origin/main
```

Rebase only a branch nobody else has pulled and that is not yet under review; then
`git push --force-with-lease`. **On someone else's branch — a human's, or a Code
Janitor PR — do neither.** Say the branch is behind or conflicted and leave the
update to its author.

Two conflict shapes here have a right answer that is not "pick a side":

- **`.sqlx/`** — never resolve a descriptor by hand. Take either side, then
  regenerate: `make prepare` against a database with every migration applied, and
  commit what it writes.
- **`migrations/`** — two branches adding migrations do not really conflict; both
  files stay, and the timestamps order them. What is *not* allowed is editing either
  one to make them agree (constraint 3 in `CLAUDE.md`: sqlx checksums applied
  migrations, so an edit fails the next production boot, not this build).

A conflicted hunk where both sides changed the same logic is not yours to guess at —
say which file and hunk needs its author.

## Validate before you push

Run what CI runs. `./validate.sh` is exactly that, in CI's order, plus the constraint
checks CI does not have:

```bash
./validate.sh
```

It starts Postgres and Redis if they are down — both are installed on the standard
container image and merely stopped, and around 190 of the tests need a real Postgres or they
fail rather than skip. If you would rather run the steps by hand, run all of them,
because CI stops at the first failure and a green step one says nothing about step
five:

```bash
cargo clippy -- -D warnings
cargo clippy --features dev-auth -- -D warnings
make test
make test-dev-auth
```

Do not add `--all-targets`: CI does not lint the test code, and the test code does
not currently pass with it, so a finding from there is not the failure you are
chasing and "fixing" it is a large diff nobody asked for.

For a CI fix, reproduce the failure locally first and then show the same command
passing. One validated push beats three speculative ones.

## What a fix here is never allowed to be

Beyond the standing rules about tests and CI config:

- **No `#[allow(...)]` to get past a clippy finding**, and no lint configuration
  added to `Cargo.toml` for it. The finding is about the code. The only exception is
  the one clippy itself suggests where the alternative is genuinely wrong — and that
  wants a comment saying why, in the same commit.
- **No hand-edited `.sqlx/` descriptor.** They are generated. `make prepare`.
- **No edit to an existing migration**, for any reason, including a typo. Add a new
  one. See `CLAUDE.md` constraint 3 for what the alternative costs: a rollout that
  refuses to boot after the image is pushed.
- **No new `.cargo/audit.toml` ignore** to silence `cargo audit` unless the advisory
  genuinely does not apply to this service, and then only with the reasoning and the
  condition that retires the entry, both written down. An ignore with neither is how
  a real vulnerability gets parked forever.
- **Nothing that turns on `dev-auth` outside local development.** If a test only
  passes with the feature on, that is the finding, not the fix.
- **No secret in `k8s/`**, and no relaxing of a rate limit or guardrail default to
  make a test pass — the compiled-in defaults are the production values, because the
  manifests set only eleven of the ~forty environment variables the code reads.

The two-identity split (`context/2026-09-05_user_identity_derivation.md`) is the
trap that looks like a bug: `parse_or_hash_uuid` deliberately keys `configs`,
`drawings` and `devices` differently from `users` and `sessions`, and
`src/routes/sync/tests/identity.rs` pins that on purpose. If a fix would move it, it
is not a fix — it orphans every existing row. Say so instead.

## Commits

Match the log. Titles here are a plain sentence in the imperative naming the
behaviour change — "Hash refresh tokens with SHA-256, and read the old format too",
"Collapse the grocery item writes into one statement per run" — not "fix CI" or
"address review". The body explains *why*, in prose, including for a one-line CI fix:
name the step, the lint or the test, and what was actually wrong. Keep each fix
minimal: what the failure or the comment needs, and no more.

## Worktrees

One worktree, one task, one PR (`CLAUDE.md`). If you need a checkout of a PR that is
not the one this session already has, take a scratch worktree and remove it on every
exit path; never repoint the session's own working tree at someone else's branch.
