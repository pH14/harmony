#!/usr/bin/env bash
# Bounded remote-sentinel waiter. Every wait has a ceiling and a loud timeout
# branch: a waiter that can sleep forever is how a box sits idle behind a shell
# that already missed its signal (the D83 lesson, on the operator's side).
#
#   waiter.sh <log-path> <sentinel-text> [max-minutes]
#
# Exits 0 and prints SENTINEL_SEEN when the sentinel appears; exits 2 and prints
# WAITER_TIMEOUT when the ceiling is reached. It never exits silently.
set -uo pipefail
log="${1:?usage: waiter.sh <log-path> <sentinel> [max-minutes]}"
sentinel="${2:?missing sentinel text}"
max_minutes="${3:-90}"
deadline=$(( $(date +%s) + max_minutes * 60 ))
while :; do
    if ssh msr1 "grep -q '${sentinel}' '${log}'" 2>/dev/null; then
        echo "SENTINEL_SEEN ${sentinel} in ${log}"
        exit 0
    fi
    if [ "$(date +%s)" -ge "$deadline" ]; then
        echo "WAITER_TIMEOUT after ${max_minutes}m waiting for ${sentinel} in ${log}"
        echo "ACTION REQUIRED: check the box directly; do not assume the job is still running."
        exit 2
    fi
    sleep 30
done
