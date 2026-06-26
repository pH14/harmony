#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# One-glance status of all delegated workers: tmux sessions, lifecycle markers,
# and each task branch's latest commit.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== tmux sessions"
tmux ls 2>/dev/null | grep '^agent-' || echo "  (none)"

echo "== markers (/tmp/harmony-agents)"
# shellcheck disable=SC2012  # human-readable listing; marker names are ours
ls -lt /tmp/harmony-agents/ 2>/dev/null | tail -n +2 | head -20 || true
[[ -f /tmp/harmony-agents/events.log ]] && { echo "-- last events:"; tail -5 /tmp/harmony-agents/events.log; }

echo "== task branches"
git for-each-ref --sort=-committerdate --format='  %(refname:short)  %(committerdate:relative)  %(subject)' 'refs/heads/task/*' | head -10
