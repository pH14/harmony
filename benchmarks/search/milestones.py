#!/usr/bin/env python3
"""Collect executions to each Metroid milestone per build, seed and cell.

Each source is either a matrix directory written by `eval.py run` or a results
JSON written by `eval.py` holding cell summaries under `rows`. For every
Metroid cell the last progress record supplies `named_progress.first_seen`,
which is cumulative, so the tail of `campaign/progress.jsonl` is read when it
is present and the summary's own copy is used otherwise.
"""

import argparse
import json
import sys
from pathlib import Path

MILESTONES = [
    "brinstar",
    "morph_ball",
    "missile_capacity",
    "norfair",
    "energy_tank",
    "bombs",
    "long_beam",
    "ice_beam",
    "high_jump",
    "varia_suit",
    "wave_beam",
    "screw_attack",
    "kraid_area",
    "kraid_defeated",
    "ridley_area",
    "ridley_defeated",
    "tourian",
    "tourian_corridor",
    "tourian_far",
    "tourian_bottom",
    "tourian_approach",
    "tourian_end",
    "zebetite_destroyed",
    "mother_brain_room",
    "mother_brain_defeated",
    "escape_started",
    "ending",
]


def last_line(path, window=1 << 20):
    with path.open("rb") as handle:
        handle.seek(0, 2)
        size = handle.tell()
        read = min(size, window)
        handle.seek(size - read)
        tail = handle.read(read)
    for line in reversed(tail.split(b"\n")):
        if not line.strip():
            continue
        try:
            return json.loads(line)
        except json.JSONDecodeError:
            continue
    return None


def progress_of(summary, cell_dir):
    if cell_dir is not None:
        log = cell_dir / "campaign" / "progress.jsonl"
        if log.is_file() and log.stat().st_size > 0:
            record = last_line(log)
            if record is not None:
                return record
    if summary.get("last_progress"):
        return summary["last_progress"]
    result = summary.get("result") or {}
    return {
        "executions": result.get("executions"),
        "milestones": result.get("milestones"),
        "workload_diagnostics": {"named_progress": summary.get("named_progress") or {}},
    }


def row_of(summary, cell_dir):
    identity = summary.get("identity") or {}
    request = summary.get("search_request") or {}
    progress = progress_of(summary, cell_dir) or {}
    diagnostics = progress.get("workload_diagnostics") or {}
    named = (diagnostics.get("named_progress") or {}).get("first_seen") or {}
    first = {
        name: (named[name] or {}).get("execution")
        for name in MILESTONES
        if named.get(name)
    }
    build = (summary.get("build") or {}).get("binary_sha256")
    policies = identity.get("policies") or {}
    return {
        "cell": summary.get("cell"),
        "arm": summary.get("arm") or summary.get("case"),
        "case": summary.get("case"),
        "status": summary.get("status"),
        "exit_code": summary.get("exit_code"),
        "build": build[:12] if build else None,
        "seed": request.get("seed") or identity.get("seed"),
        "workers": request.get("workers") or identity.get("workers"),
        "memory_mib": request.get("memory_mib") or identity.get("memory_mib"),
        "executions_budget": request.get("executions") or identity.get("executions"),
        "executions_reached": progress.get("executions"),
        "selector": request.get("selector") or identity.get("selector"),
        "mixture": request.get("mixture") or identity.get("mixture"),
        "key_policy": policies.get("key_policy"),
        "root_input": request.get("root_input"),
        "milestones": progress.get("milestones"),
        "first_execution": first,
    }


def is_metroid(summary):
    request = summary.get("search_request") or {}
    identity = summary.get("identity") or {}
    return (request.get("game") or identity.get("game")) == "metroid"


def collect(source):
    source = Path(source)
    if source.is_file():
        document = json.loads(source.read_text())
        rows = document.get("rows") or []
        return source.stem, [row_of(row, None) for row in rows if is_metroid(row)]
    rows = []
    for cell in sorted(source.iterdir()):
        summary = cell / "summary.json"
        if not summary.is_file():
            continue
        document = json.loads(summary.read_text())
        if is_metroid(document):
            rows.append(row_of(document, cell))
    return source.name, rows


def render(name, rows, columns):
    print(f"== {name} ({len(rows)} cells)")
    header = ["arm", "cell", "build", "reached", "status"] + columns
    widths = [len(part) for part in header]
    table = []
    for row in rows:
        line = [
            str(row["arm"]),
            str(row["cell"]),
            str(row["build"]),
            f"{row['executions_reached']:,}" if row["executions_reached"] else "-",
            str(row["status"]),
        ]
        for column in columns:
            hit = row["first_execution"].get(column)
            line.append(f"{hit:,}" if hit else "-")
        widths = [max(width, len(part)) for width, part in zip(widths, line)]
        table.append(line)
    for line in [header] + table:
        print("  ".join(part.ljust(width) for part, width in zip(line, widths)))
    print()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("sources", nargs="+")
    parser.add_argument("--out", type=Path)
    arguments = parser.parse_args()
    for source in arguments.sources:
        name, rows = collect(source)
        if not rows:
            print(f"== {name}: no Metroid cells", file=sys.stderr)
            continue
        reached = sorted(
            {
                milestone
                for row in rows
                for milestone in row["first_execution"]
            },
            key=MILESTONES.index,
        )
        render(name, rows, reached)
        if arguments.out:
            arguments.out.mkdir(parents=True, exist_ok=True)
            document = {
                "format": "harmony-metroid-milestones-v1",
                "matrix": name,
                "milestones": MILESTONES,
                "cells": rows,
            }
            path = arguments.out / f"{name}.json"
            path.write_text(json.dumps(document, indent=1, sort_keys=True) + "\n")
            print(f"wrote {path}", file=sys.stderr)


if __name__ == "__main__":
    main()
