#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Enforce the frozen first-milestone limits on every production sample."""
import json
from pathlib import Path
import sys

samples = json.loads(Path(sys.argv[1]).read_text())["samples"]
if len(samples) != 11:
    raise SystemExit("eleven production samples are required")
for sample in samples:
    for key, limit in {"run": 0.5, "capture": 0.015, "fresh_restore": 0.1, "serialized_bytes": 17825792}.items():
        if sample[key] > limit:
            raise SystemExit(f"frozen budget exceeded: {key}={sample[key]} > {limit}")
print("all production samples satisfy frozen budgets")

if len(sys.argv) > 2:
    rows = [json.loads(line.removeprefix("WASM_MEMORY_SAMPLES ")) for line in Path(sys.argv[2]).read_text().splitlines() if line.startswith("WASM_MEMORY_SAMPLES ")]
    if {(row["pages"], row["history"]) for row in rows} != {(pages, history) for pages in (1, 16, 256) for history in (0, 1000, 10000)}:
        raise SystemExit("all nine memory/history cases are required")
    for row in rows:
        for key, limit in {"capture": 0.015, "fresh_restore": 0.1}.items():
            if len(row[key]) != 11 or max(row[key]) > limit:
                raise SystemExit(f"frozen scaling budget exceeded: {row}")
    print("all memory/history samples satisfy frozen budgets")
