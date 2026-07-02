#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Spawn a delegated-task Claude Code worker in its own git worktree + tmux session.
#
# Usage:  scripts/agent-spawn.sh <task-slug> [--engine claude|deepseek] [--model ID] [--yolo]
#   <task-slug>  matches a tasks/*<slug>*.md spec (e.g. "vtime", "snapshot-store")
#   --engine     deepseek routes the worker through DeepSeek's Anthropic-compatible
#                endpoint (requires DEEPSEEK_API_KEY in the environment)
#   --model      worker model id (default: claude-opus-4-8 — Opus 4.8, the baseline for
#                ordinary tasks). Pass --model claude-fable-5 to route a high-complexity
#                task (deep architectural reasoning, cross-crate refactors, gnarly
#                determinism bugs) to Fable 5 instead, or --model claude-sonnet-5 for a
#                quick/simple task (docs, small mechanical fixes, low-risk cleanup).
#                Ignored when --engine deepseek (DeepSeek picks the model).
#   --perm       permission mode (default: auto — the classifier auto-approves low-risk
#                commands and blocks risky ones; good for unattended/foreman runs).
#                Other values: acceptEdits, default, bypassPermissions.
#   --yolo       --dangerously-skip-permissions (bypass ALL checks; dev box only)
#
# The worker runs interactively inside tmux session "agent-<slug>":
#   watch:   tmux attach -t agent-<slug>     (detach: ctrl-b d)
#   steer:   scripts/agent-send.sh <slug> "message"
#   done?:   /tmp/harmony-agents/harmony-task-<slug>.stop appears whenever the
#            worker ends a turn (Stop hook); .session-end when the session closes.
set -euo pipefail
cd "$(dirname "$0")/.."

SLUG="${1:?usage: agent-spawn.sh <task-slug> [--engine claude|deepseek] [--model ID] [--yolo]}"
shift
ENGINE=claude
MODEL=claude-opus-4-8          # Opus 4.8 — baseline model; --model claude-fable-5 for high-complexity,
                                # --model claude-sonnet-5 for quick/simple tasks
# auto: classifier auto-approves low-risk commands, blocks risky ones — unattended-friendly
PERMFLAGS="--permission-mode auto"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --engine) ENGINE="$2"; shift 2 ;;
        --model)  MODEL="$2";  shift 2 ;;
        --perm)   PERMFLAGS="--permission-mode $2"; shift 2 ;;
        --yolo)   PERMFLAGS="--dangerously-skip-permissions"; shift ;;
        *) echo "unknown argument: $1" >&2; exit 1 ;;
    esac
done

TASKFILE=$(find tasks -name "*${SLUG}*.md" ! -name "00-*" | sort | head -1)
[[ -n "$TASKFILE" ]] || { echo "no task spec matching '$SLUG' in tasks/" >&2; exit 1; }

WTNAME="harmony-task-$SLUG"
WT="../$WTNAME"
BRANCH="task/$SLUG"
SESSION="agent-$SLUG"

tmux has-session -t "$SESSION" 2>/dev/null && { echo "session $SESSION already exists" >&2; exit 1; }

if ! git worktree list --porcelain | grep -q "/$WTNAME\$"; then
    git worktree add "$WT" -b "$BRANCH" 2>/dev/null || git worktree add "$WT" "$BRANCH"
fi

rm -f "/tmp/harmony-agents/$WTNAME".*
mkdir -p /tmp/harmony-agents

cat > "$WT/.agent-prompt.md" <<EOF
You are a delegated implementation worker for the harmony project.

Your task spec is $TASKFILE — read it AND tasks/00-CONVENTIONS.md in full before
writing any code. Implement the task until every acceptance gate passes.

You are already in your dedicated worktree on branch $BRANCH (do not create another).
Commit as you go with clear messages. When all gates are green, write your
IMPLEMENTATION.md (per conventions), commit, and stop. Do not push.
EOF

# caffeinate (macOS): keep the machine from idle-sleeping while a worker runs
CAFF=""
command -v caffeinate >/dev/null && CAFF="caffeinate -i "
# Default (claude) engine pins the model explicitly (Opus 4.8 baseline, or Fable 5 /
# Sonnet 5 via --model for high-complexity / quick-simple tasks); deepseek ignores it.
MODELFLAG="--model $MODEL"
if [[ "$ENGINE" == deepseek ]]; then
    : "${DEEPSEEK_API_KEY:?--engine deepseek requires DEEPSEEK_API_KEY}"
    MODELFLAG=""
elif [[ "$ENGINE" != claude ]]; then
    echo "unknown engine: $ENGINE" >&2; exit 1
fi

# Disable Claude Code's prompt-suggestion ghost text: in a detached tmux pane it
# renders as dim pre-typed-looking input on the ❯ line, and a stray Tab+Enter
# would submit the model-invented instruction to the worker. (Debugged 2026-07-02.)
CMD="CLAUDE_CODE_ENABLE_PROMPT_SUGGESTION=0 ${CAFF}claude $PERMFLAGS $MODELFLAG \"\$(cat .agent-prompt.md)\"; echo; echo '[claude exited — pane kept for inspection]'; exec bash"
if [[ "$ENGINE" == deepseek ]]; then
    CMD="export ANTHROPIC_BASE_URL=https://api.deepseek.com/anthropic ANTHROPIC_AUTH_TOKEN=$DEEPSEEK_API_KEY; $CMD"
fi

tmux new-session -d -s "$SESSION" -c "$WT" "$CMD"
echo "spawned $SESSION (engine=$ENGINE, model=$MODEL, branch=$BRANCH, spec=$TASKFILE)"
echo "  attach: tmux attach -t $SESSION"
echo "  marker: /tmp/harmony-agents/$WTNAME.stop"
