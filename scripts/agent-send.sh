#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Send a message to a running worker's Claude Code prompt (typed + submitted).
# Usage: scripts/agent-send.sh <task-slug> <message...>
set -euo pipefail
SLUG="${1:?usage: agent-send.sh <task-slug> <message...>}"
shift
MSG="${*:?message required}"
SESSION="agent-$SLUG"
WTNAME="harmony-task-$SLUG"

tmux has-session -t "$SESSION" 2>/dev/null || { echo "no session $SESSION" >&2; exit 1; }
# Clear the stop marker so its reappearance means "responded to THIS message".
rm -f "/tmp/harmony-agents/$WTNAME.stop"
tmux send-keys -t "$SESSION" -l -- "$MSG"
sleep 0.5
tmux send-keys -t "$SESSION" Enter
echo "sent to $SESSION; watch /tmp/harmony-agents/$WTNAME.stop"
