#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Lakko Contributors
#
# PreToolUse hook (Bash matcher): block the AI agent from running
# `git commit` / `git push`. The maintainer authors every commit; the agent
# must hand over the commit message + the exact `git add` list instead and let
# the maintainer run `git commit`. This is the harness-enforced backstop for
# the rule the agent kept ignoring (CLAUDE.md "The maintainer authors the
# commit" + memory/feedback_never-commit-autonomously.md).
#
# Only the agent's Bash tool calls pass through this hook; the maintainer's own
# terminal `git commit` is unaffected.
#
# Reads the PreToolUse event JSON on stdin; emits a "deny" decision (and exits
# 0) when the command invokes the `commit` or `push` git subcommand; otherwise
# emits nothing and exits 0 (allow).

set -euo pipefail

COMMAND="$(jq -r '.tool_input.command // empty')"

# Match `git` followed by zero+ global options (`-x` flags or `key=val`
# config), then the `commit` or `push` SUBCOMMAND as a whole word. This catches
# `git commit`, `git commit -m ...`, `git push`, `git push origin main`,
# `git -c user.name=x commit`, but NOT `git log --grep=commit` (there the first
# token after `git` is `log`, not commit/push) nor the plumbing `commit-tree`.
if printf '%s' "$COMMAND" \
  | grep -qE '\bgit\b([[:space:]]+-[^[:space:]]+|[[:space:]]+[A-Za-z_.-]+=[^[:space:]]+)*[[:space:]]+(commit|push)([[:space:]]|$)'; then
  jq -n '{
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "deny",
      permissionDecisionReason: "git commit/push is blocked by a local hook. The maintainer authors every commit. Do NOT commit. Instead, end your reply with the ready-to-paste commit message + the exact `git add` list, and let the maintainer run git commit."
    }
  }'
  exit 0
fi

exit 0
