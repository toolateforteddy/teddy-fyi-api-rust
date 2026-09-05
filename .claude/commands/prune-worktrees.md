---
description: Remove worktrees and branches whose work is already merged into origin/main
---

Clean up finished worktrees in this repo.

1. Run `git fetch origin`, then `git worktree list` and, for every local branch
   matching `claude/*`, record: the worktree holding it (if any), how many commits it
   is behind `origin/main`, whether it has unpushed commits, whether its worktree has
   uncommitted changes, and whether `git diff --quiet origin/main...<branch>` reports
   no unique content.

   PRs land here as merge commits rather than squashes, so `git branch --merged
   origin/main` is meaningful — but check the content diff too, since a branch that
   was merged *into* and then merged *from* can look either way.

2. Present a short table of the candidates -- branches with no unique content AND no
   uncommitted changes in their worktree. List separately, without proposing removal,
   any branch that still has unique content or a dirty worktree, and say why it was
   spared.

3. Ask the user to confirm which candidates to remove. Do not remove anything before
   they answer, and never remove the worktree the current session is running in.

4. For each confirmed entry run `git worktree remove <path>` then
   `git branch -d <branch>` (`-D` only if `-d` refuses and the content diff is
   genuinely empty). If `git worktree remove` refuses, report why and move on rather
   than forcing.

5. Finish with `git worktree prune` and a one-line summary of what was removed and
   what was kept.
