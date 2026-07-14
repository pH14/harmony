#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Spawn a fixer worker to take over an EXISTING PR on its own head branch — used by the
# foreman to drive an out-of-band PR (one not created from a tasks/*.md spec) through the
# review→fix cycle. Unlike agent-spawn.sh, this needs no task spec; it works on whatever
# branch the PR is on.
#
# Usage:  scripts/agent-takeover.sh <pr-number> [--model ID] [--perm MODE]
#   watch:  tmux attach -t agent-pr<N>      (detach: ctrl-b d)
#   steer:  scripts/agent-send.sh pr<N> "message"
#   done?:  /tmp/harmony-agents/harmony-pr<N>.stop appears when the worker ends a turn
#
# Use this ONLY for PRs whose fixes are crate code. For docs/spec-only PRs the foreman edits
# the branch directly (it owns docs/specs) — no worker needed.
set -euo pipefail
cd "$(dirname "$0")/.."

PR="${1:?usage: agent-takeover.sh <pr-number> [--model ID] [--effort low|medium|high|xhigh|max] [--perm MODE]}"
shift || true
MODEL=claude-opus-4-8
EFFORT=""                      # empty = per-model default below (Paul's 2026-07-14 ruling)
PERMFLAGS="--permission-mode auto"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --model)  MODEL="$2";  shift 2 ;;
        --effort) EFFORT="$2"; shift 2 ;;
        --perm)   PERMFLAGS="--permission-mode $2"; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 1 ;;
    esac
done

# Same per-model effort defaults as agent-spawn.sh: fixers think hard too.
if [[ -z "$EFFORT" ]]; then
    case "$MODEL" in
        claude-sonnet-5) EFFORT=high ;;
        *)               EFFORT=xhigh ;;
    esac
fi

BRANCH=$(gh pr view "$PR" --json headRefName --jq .headRefName)
[[ -n "$BRANCH" ]] || { echo "could not resolve head branch for PR #$PR" >&2; exit 1; }
URL=$(gh pr view "$PR" --json url --jq .url)

SLUG="pr$PR"
WTNAME="harmony-$SLUG"
WT="../$WTNAME"
SESSION="agent-$SLUG"

tmux has-session -t "$SESSION" 2>/dev/null && { echo "session $SESSION already exists" >&2; exit 1; }

git fetch origin "$BRANCH" -q
if ! git worktree list --porcelain | grep -q "/$WTNAME\$"; then
    # Check out the PR's existing branch into a dedicated worktree (track origin).
    git worktree add --track -b "$BRANCH" "$WT" "origin/$BRANCH" 2>/dev/null \
        || git worktree add "$WT" "$BRANCH"
fi

mkdir -p /tmp/harmony-agents
rm -f "/tmp/harmony-agents/$WTNAME".*

cat > "$WT/.agent-prompt.md" <<EOF
You are a fixer worker taking over GitHub PR #$PR ($URL) for the harmony project.

You are already in a dedicated worktree on the PR's head branch ($BRANCH) — do NOT create
another branch. First read, in full: this PR and ALL its review comments
(\`gh pr view $PR --comments\`), tasks/00-CONVENTIONS.md, and AGENTS.md. If the PR body or a
comment links a task spec or a decision doc, read that too — it defines what "correct" means.

Then: fix every **[blocking]** finding, answer each **[question]** in a PR reply, re-run all
gates (build, test, clippy -D warnings, fmt, plus any quality gates the change touches),
commit with clear messages, and STOP. Do NOT push — the foreman pushes and merges. Keep the
change in this PR's scope; do not refactor unrelated code.
EOF

CAFF=""
command -v caffeinate >/dev/null && CAFF="caffeinate -i "
# CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION=0: same ghost-text hazard as agent-spawn.sh —
# takeover sessions were missing it (that is why agent-pr98 showed phantom ❯ suggestions).
CMD="CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION=0 ${CAFF}claude $PERMFLAGS --model $MODEL --effort $EFFORT \"\$(cat .agent-prompt.md)\"; echo; echo '[claude exited — pane kept for inspection]'; exec bash"

tmux new-session -d -s "$SESSION" -c "$WT" "$CMD"
echo "spawned $SESSION (model=$MODEL, branch=$BRANCH, PR=#$PR)"
echo "  attach: tmux attach -t $SESSION"
echo "  marker: /tmp/harmony-agents/$WTNAME.stop"
