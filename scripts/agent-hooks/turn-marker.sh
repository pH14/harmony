#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Claude Code hook: record worker lifecycle markers for the orchestrator.
# Wired up in .claude/settings.json (Stop / SessionEnd events); runs inside each
# worker session's cwd, so $PWD's basename identifies the worktree/agent.
#
# Markers (under /tmp/harmony-agents/):
#   <worktree>.stop         touched when the worker finishes a turn (awaiting input)
#   <worktree>.session-end  touched when the worker session ends
#   events.log              append-only "<epoch> <worktree> <event>" log
set -u
EVENT="${1:-unknown}"
DIR=/tmp/harmony-agents
NAME=$(basename "$PWD")
mkdir -p "$DIR"
cat > /dev/null # drain the hook's stdin JSON; we only need event + cwd
printf '%s %s %s\n' "$(date +%s)" "$NAME" "$EVENT" >> "$DIR/events.log"
touch "$DIR/$NAME.$EVENT"
exit 0
