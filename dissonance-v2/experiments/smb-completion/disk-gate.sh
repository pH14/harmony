#!/usr/bin/env bash
# Refuse to launch a campaign when free space is too low to hold its output.
#
# A link writes roughly 13-15 GB of archive and report. Launching under that
# leaves a run to die partway and take the slot with it, so the check is a
# precondition rather than a warning.
set -uo pipefail
min_gb="${1:-30}"
free_gb=$(df -BG --output=avail /root | tail -1 | tr -dc '0-9')
if [ "${free_gb:-0}" -lt "$min_gb" ]; then
    echo "DISK GATE FAILED: ${free_gb} GB free, ${min_gb} GB required."
    echo "REFUSING TO LAUNCH. Free space or compress old archives first."
    exit 2
fi
echo "disk gate ok: ${free_gb} GB free"
