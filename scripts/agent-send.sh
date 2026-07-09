#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Send a message to a running worker's Claude Code prompt (typed + submitted),
# then verify it submitted (the input line no longer holds our text).
# NOTE: dim text on the idle ❯ line is Claude Code ghost-suggestion UI, not
# input — never Tab/Enter it; this script types over it safely. (2026-07-02.)
set -euo pipefail
SLUG="${1:?usage: agent-send.sh <task-slug> <message...>}"
shift
MSG="${*:?message required}"
SESSION="agent-$SLUG"
WTNAME="harmony-task-$SLUG"

tmux has-session -t "$SESSION" 2>/dev/null || { echo "no session $SESSION" >&2; exit 1; }

# LANDMINE GUARD (2026-07-07): if the worker is showing an AskUserQuestion menu
# ("Enter to select"), typed text is swallowed and the trailing Enter SELECTS the
# highlighted default — the worker then believes the user answered. Never type into
# a menu: the sender must Esc to the idle prompt (or answer the menu deliberately)
# first. See memory agent-send-menu-landmine.
if tmux capture-pane -p -t "$SESSION" | grep -qF 'Enter to select'; then
    echo "REFUSING: $SESSION is showing a selection menu — sending now would pick the" >&2
    echo "highlighted default, not deliver your message. Esc it to the idle prompt first." >&2
    exit 2
fi

# Clear the stop marker so its reappearance means "responded to THIS message".
rm -f "/tmp/harmony-agents/$WTNAME.stop"

# Plain-text content of the input box (last ❯ line), dim ghost spans stripped.
input_line() {
    tmux capture-pane -e -p -t "$SESSION" \
      | grep -F -- $'❯' | tail -1 \
      | sed -E $'s/\x1b\\[2m[^\x1b]*//g; s/\x1b\\[[0-9;]*m//g; s/.*❯//'
}

FPR="${MSG:0:40}"   # the input box shows the message head even when wrapped

for attempt in 1 2 3; do
    tmux send-keys -t "$SESSION" C-u          # drop any real leftover input; never submits
    tmux send-keys -t "$SESSION" -l -- "$MSG"
    sleep 0.5
    tmux send-keys -t "$SESSION" Enter
    sleep 1
    input_line | grep -qF -- "$FPR" || { echo "sent to $SESSION; watch /tmp/harmony-agents/$WTNAME.stop"; exit 0; }
    echo "attempt $attempt: input line still shows the message; retrying Enter" >&2
    tmux send-keys -t "$SESSION" Enter
    sleep 1
    input_line | grep -qF -- "$FPR" || { echo "sent to $SESSION (after Enter retry); watch /tmp/harmony-agents/$WTNAME.stop"; exit 0; }
done
echo "FAILED: $SESSION still shows the message unsubmitted after 3 attempts" >&2
exit 1
