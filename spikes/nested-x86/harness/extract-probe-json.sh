#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# nested-x86: extract + VALIDATE the N-0 probe JSON from a sentinel-wrapped
# artifact (PR #98 round-5 P2). The retained `probe.json` files (and the raw
# console) carry the probe output between NESTED_X86_PROBE_BEGIN/END sentinels,
# possibly interleaved with kernel printk lines — they are deliberately NOT
# rewritten (golden evidence is immutable). This is the consumer seam: it
# strips the sentinels and printk lines, validates the remainder as JSON, and
# prints the validated JSON to stdout (exit 1 on invalid).
#
# Usage: extract-probe-json.sh <probe.json | console.log>
set -euo pipefail
IN="${1:?path to a sentinel-wrapped probe.json or console.log}"
awk '/NESTED_X86_PROBE_END/{f=0} f{print} /NESTED_X86_PROBE_BEGIN/{f=1}' "$IN" \
  | grep -v '^\[ *[0-9]*\.[0-9]*\]' \
  | python3 -m json.tool
