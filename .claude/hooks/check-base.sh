#!/usr/bin/env bash
# SessionStart hook: keep parallel worktrees honest about their base.
#
# Fetches origin, then tells Claude (and the user) if this working tree is behind
# the default branch, or if its branch is already merged and the worktree is
# finished. Always exits 0 -- a stale base is a warning, not an error. Messages
# deliberately avoid " and backslashes so they can be dropped into JSON without
# escaping.

set -uo pipefail

git rev-parse --git-dir >/dev/null 2>&1 || exit 0

git fetch origin --quiet 2>/dev/null

base=$(git symbolic-ref --quiet --short refs/remotes/origin/HEAD 2>/dev/null)
[ -n "$base" ] || base=origin/main
git rev-parse --verify --quiet "$base" >/dev/null 2>&1 || exit 0

branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null)
behind=$(git rev-list --count "HEAD..$base" 2>/dev/null) || exit 0
[ "${behind:-0}" -gt 0 ] || exit 0

dirty=$(git status --porcelain 2>/dev/null | head -1)

if [ -z "$dirty" ] && git diff --quiet "$base...HEAD" 2>/dev/null; then
  msg="This worktree ($branch) is behind $base by $behind commits and has no unique content of its own -- its work is already merged, so the worktree is finished. Do not start new work here. If the user asks for a new task anyway, first run: git switch -c claude/TASK $base"
else
  msg="This worktree ($branch) is behind $base by $behind commits. Before starting NEW work here, branch fresh with: git switch -c claude/TASK $base -- or, to bring this in-progress branch up to date instead: git fetch origin && git merge $base (this repo lands PRs as merge commits, so merging main in is the house style; rebase only a branch nobody has pulled)."
fi

printf '{"systemMessage":"%s","hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"%s"}}\n' "$msg" "$msg"
