#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Score a segment panel: work to each segment's milestone, per arm and seed.

    segments.py MANIFEST control=MATRIX [candidate=MATRIX ...] [--out FILE]

MANIFEST is the suite `eval.py run` executed. Each case names its milestone:

    {"named": "bombs"}             a `named_progress.first_seen` entry is set
    {"named": ["bombs", "ice_beam"]}
                                   any of the listed entries is set
    {"objective": true}            the campaign reached its objective
    {"at_least": {"progress.world": 4, "progress.level": 0}}
                                   the listed progress fields, compared in
                                   order, reach the listed values

Each MATRIX is a directory written by `eval.py run` over that manifest. The
first arm is the control. A cell's time to the milestone is the work and the
tries on the first line of `campaign/progress.jsonl` that shows it. A cell that
never shows it is censored at its last line.

With one arm the table shows how many seeds passed each segment, which is what
calibrating a budget needs. With more arms every later arm is compared with
the control by a log-rank test per segment and a stratified log-rank test
across segments.
"""

import argparse
import json
import math
import re
import sys
from pathlib import Path

CELL = re.compile(r"^(?P<case>.+)-s(?P<seed>\d+)-w\d+-m\d+$")
CALIBRATED_SHARE = (0.3, 0.7)
SIGNIFICANT = 0.05


def lookup(record, path):
    value = record
    for part in path.split("."):
        if not isinstance(value, dict):
            return None
        value = value.get(part)
    return value


def reached(milestone, record):
    if "named" in milestone:
        seen = lookup(record, "workload_diagnostics.named_progress.first_seen") or {}
        return any(seen.get(name) is not None for name in names(milestone))
    if milestone.get("objective"):
        return (record.get("objectives_reached") or 0) >= 1
    if "at_least" in milestone:
        wanted = milestone["at_least"]
        have = [lookup(record, path) for path in wanted]
        if any(type(value) not in (int, float) for value in have):
            return False
        return have >= list(wanted.values())
    raise ValueError(f"unknown milestone: {milestone}")


def names(milestone):
    named = milestone["named"]
    return [named] if isinstance(named, str) else named


def describe(milestone):
    if not isinstance(milestone, dict) or len(milestone) != 1:
        raise ValueError(f"a milestone names exactly one of named, objective, at_least: {milestone}")
    named = milestone.get("named")
    if isinstance(named, str) or (isinstance(named, list) and named and all(isinstance(name, str) for name in named)):
        return " or ".join(names(milestone))
    if milestone.get("objective") is True:
        return "objective"
    wanted = milestone.get("at_least")
    if not isinstance(wanted, dict) or not wanted or any(type(value) not in (int, float) for value in wanted.values()):
        raise ValueError(f"unknown milestone: {milestone}")
    return ",".join(f"{path.rsplit('.', 1)[-1]}>={value}" for path, value in wanted.items())


def score_cell(progress_path, milestone):
    """Returns (work, tries, passed), or None when the cell wrote no progress."""
    last = None
    with progress_path.open() as lines:
        for line in lines:
            if not line.strip():
                continue
            record = json.loads(line)
            last = record
            if reached(milestone, record):
                return record["execution_work"], record["executions"], True
    if last is None:
        return None
    return last["execution_work"], last["executions"], False


def read_arm(matrix, cases):
    """Maps case id to a list of {seed, work, tries, passed}, in seed order."""
    rows = {case: [] for case in cases}
    for cell in sorted(matrix.iterdir()):
        match = CELL.match(cell.name)
        if not match or match["case"] not in cases:
            continue
        progress = cell / "campaign" / "progress.jsonl"
        scored = score_cell(progress, cases[match["case"]]) if progress.is_file() else None
        if scored is None:
            print(f"no progress in {cell}", file=sys.stderr)
            continue
        work, tries, passed = scored
        rows[match["case"]].append({"seed": int(match["seed"]), "work": work, "tries": tries, "passed": passed})
    for cells in rows.values():
        cells.sort(key=lambda row: row["seed"])
    return rows


def median_work(rows):
    """Kaplan-Meier median work, or None when fewer than half the seeds are known to pass by any work."""
    at_risk = len(rows)
    survival = 1.0
    for work in sorted({row["work"] for row in rows}):
        events = sum(1 for row in rows if row["work"] == work and row["passed"])
        if at_risk and events:
            survival *= 1 - events / at_risk
            if survival <= 0.5:
                return work
        at_risk -= sum(1 for row in rows if row["work"] == work)
    return None


def log_rank(control, candidate):
    """Returns (observed - expected, variance) of candidate passes under no difference between arms."""
    difference = 0.0
    variance = 0.0
    both = [(row, 0) for row in control] + [(row, 1) for row in candidate]
    for work in sorted({row["work"] for row, _ in both if row["passed"]}):
        at_risk = [arm for row, arm in both if row["work"] >= work]
        events = [arm for row, arm in both if row["work"] == work and row["passed"]]
        total, in_candidate, passed = len(at_risk), sum(at_risk), len(events)
        difference += sum(events) - passed * in_candidate / total
        if total > 1:
            share = in_candidate / total
            variance += passed * share * (1 - share) * (total - passed) / (total - 1)
    return difference, variance


def p_value(difference, variance):
    if variance <= 0:
        return 1.0
    return math.erfc(abs(difference) / math.sqrt(variance) / math.sqrt(2))


def verdict(difference, variance):
    if p_value(difference, variance) >= SIGNIFICANT:
        return "undecided"
    return "faster" if difference > 0 else "slower"


def summarize(rows):
    passes = sum(1 for row in rows if row["passed"])
    return {"seeds": len(rows), "passes": passes, "median_work": median_work(rows), "cells": rows}


def score(manifest, arms):
    cases = {case["id"]: case["milestone"] for case in manifest["cases"] if "milestone" in case}
    if not cases:
        raise ValueError("no case in the manifest names a milestone")
    for milestone in cases.values():
        describe(milestone)
    data = {name: read_arm(matrix, cases) for name, matrix in arms}
    control = arms[0][0]
    report = {"suite": manifest.get("id"), "control": control, "segments": [], "panel": {}}
    totals = {name: [0.0, 0.0] for name, _ in arms[1:]}
    for case, milestone in cases.items():
        segment = {"id": case, "milestone": describe(milestone), "arms": {}}
        for name, _ in arms:
            segment["arms"][name] = summarize(data[name][case])
        for name, _ in arms[1:]:
            difference, variance = log_rank(data[control][case], data[name][case])
            totals[name][0] += difference
            totals[name][1] += variance
            segment["arms"][name].update(
                p=p_value(difference, variance), verdict=verdict(difference, variance)
            )
        report["segments"].append(segment)
    for name, (difference, variance) in totals.items():
        report["panel"][name] = {"p": p_value(difference, variance), "verdict": verdict(difference, variance)}
    return report


def fmt_work(value):
    return "-" if value is None else f"{value:,}"


def render(report):
    lines = []
    names = list(report["segments"][0]["arms"])
    for segment in report["segments"]:
        lines.append(f"{segment['id']}  ({segment['milestone']})")
        for name in names:
            arm = segment["arms"][name]
            share = arm["passes"] / arm["seeds"] if arm["seeds"] else 0.0
            note = ""
            if "verdict" in arm:
                note = f"  {arm['verdict']} (p={arm['p']:.3f})"
            elif len(names) == 1 and arm["seeds"]:
                if arm["passes"] == arm["seeds"]:
                    note = "  every seed passed"
                elif CALIBRATED_SHARE[0] <= share <= CALIBRATED_SHARE[1]:
                    note = "  calibrated"
                else:
                    note = "  budget too short" if share < CALIBRATED_SHARE[0] else "  budget too long"
            lines.append(
                f"  {name:<12} {arm['passes']:>2}/{arm['seeds']:<2} passed  "
                f"median work {fmt_work(arm['median_work']):>14}{note}"
            )
    for name, panel in report["panel"].items():
        lines.append(f"panel: {name} against {report['control']}: {panel['verdict']} (p={panel['p']:.3f})")
    return "\n".join(lines)


def parse_arm(text):
    name, separator, path = text.partition("=")
    if not separator or not name or not path:
        raise argparse.ArgumentTypeError("an arm is NAME=MATRIX")
    return name, Path(path)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("arms", type=parse_arm, nargs="+", metavar="NAME=MATRIX")
    parser.add_argument("--out", type=Path, help="write the scores as JSON")
    args = parser.parse_args(argv)
    if len({name for name, _ in args.arms}) != len(args.arms):
        parser.error("arm names must differ")
    report = score(json.loads(args.manifest.read_text()), args.arms)
    print(render(report))
    if args.out:
        args.out.write_text(json.dumps(report, indent=1) + "\n")


if __name__ == "__main__":
    main()
