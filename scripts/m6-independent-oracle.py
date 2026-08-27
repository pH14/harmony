#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Independent semantic oracle for the M6 concurrency measurement."""

from __future__ import annotations

import argparse
import itertools
import json
import pathlib
import sys
from typing import Callable


def choose(schedule: tuple[int, ...], cursor: int, ready: list[int]) -> tuple[int, int]:
    if len(ready) == 1:
        return ready[0], cursor
    if cursor >= len(schedule):
        raise ValueError("schedule exhausted")
    selected = schedule[cursor]
    if selected < 0 or selected >= len(ready):
        raise ValueError("selection outside runnable set")
    return ready[selected], cursor + 1


def run_rust(schedule: tuple[int, ...]) -> tuple[bool, int]:
    steps = [0, 0]
    local = [0, 0]
    shared = 0
    cursor = 0
    while True:
        ready = [index for index, step in enumerate(steps) if step < 2]
        if not ready:
            return shared != 2, shared
        actor, cursor = choose(schedule, cursor, ready)
        if steps[actor] == 0:
            local[actor] = shared
        else:
            shared = local[actor] + 1
        steps[actor] += 1


def run_go(schedule: tuple[int, ...]) -> tuple[bool, int]:
    steps = [0, 0]
    published = False
    initialized = 0
    saw_published = False
    observed = 0xFF
    cursor = 0
    while True:
        ready = [index for index, step in enumerate(steps) if step < 2]
        if not ready:
            return saw_published and observed == 0, observed
        actor, cursor = choose(schedule, cursor, ready)
        if actor == 0:
            if steps[actor] == 0:
                published = True
            else:
                initialized = 42
        elif steps[actor] == 0:
            saw_published = published
        elif saw_published:
            observed = initialized
        steps[actor] += 1


def schedules(max_choices: int) -> list[tuple[int, ...]]:
    return list(itertools.product((0, 1), repeat=max_choices))


def verify_bug(
    result: dict[str, object],
    model: Callable[[tuple[int, ...]], tuple[bool, int]],
    max_choices: int,
) -> None:
    bug_id = str(result["id"])
    wrong = tuple(int(value) for value in result["wrong_schedule"])
    reproducer = tuple(int(value) for value in result["reproducer_schedule"])
    if model(wrong)[0] or bool(result["wrong_schedule_reproduced"]):
        raise ValueError(f"{bug_id}: wrong-schedule negative did not hold")
    bug, value = model(reproducer)
    if not bug or value != int(result["result_value"]):
        raise ValueError(f"{bug_id}: reported schedule is not an independent reproducer")
    known = [candidate for candidate in schedules(max_choices) if model(candidate)[0]]
    if not known:
        raise ValueError(f"{bug_id}: independent enumeration found no reproducer")
    if not bool(result["deterministic_replay"]):
        raise ValueError(f"{bug_id}: deterministic replay was not recorded")
    digest = str(result["transcript_sha256"])
    if len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest):
        raise ValueError(f"{bug_id}: malformed transcript digest")
    thresholds: dict[int, int] = {}
    choices: list[int] = []
    coverage = result["coverage"]
    if len(coverage) != int(result["coverage_exits"]) or not coverage:
        raise ValueError(f"{bug_id}: coverage exit report is incomplete")
    for record in coverage:
        thread = int(record["thread"])
        observed = int(record["observed"])
        ready = int(record["ready"])
        selected = int(record["selected"])
        if observed != thresholds.get(thread, 1) or ready <= 0 or selected >= ready:
            raise ValueError(f"{bug_id}: coverage threshold trace is invalid")
        thresholds[thread] = observed + 1
        if ready > 1:
            choices.append(selected)
    if choices != list(reproducer):
        raise ValueError(f"{bug_id}: coverage trace does not carry the reproducer")
    print(
        f"M6_INDEPENDENT_OK id={bug_id} "
        f"known_reproducers={len(known)} schedule={list(reproducer)}"
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("report", type=pathlib.Path)
    parser.add_argument("plan", type=pathlib.Path)
    parser.add_argument("searcher_source", type=pathlib.Path)
    parser.add_argument("--plant-schedule", choices=("rust_lost_update", "go_publish_before_init"))
    args = parser.parse_args()

    report = json.loads(args.report.read_text())
    plan_text = args.plan.read_text()
    plan = json.loads(plan_text)
    if report.get("format") != "consonance.m6-concurrency.v1":
        raise ValueError("unexpected report format")
    if plan.get("format") != "consonance.m6-plan.v1":
        raise ValueError("unexpected plan format")
    by_id = {entry["id"]: entry for entry in report["bugs"]}
    if set(by_id) != {"rust_lost_update", "go_publish_before_init"}:
        raise ValueError("report is not per-bug complete")
    go_plan = plan["go_publish_before_init"]
    if "schedule" in go_plan:
        raise ValueError("held-out plan contains a reproducing schedule fixture")
    budget = int(go_plan["budget"])
    seed = int(go_plan["seed"])
    max_choices = int(go_plan["max_choices"])
    first = tuple((seed >> shift) & 1 for shift in range(max_choices))
    if run_go(first)[0]:
        raise ValueError("held-out seed encodes an immediate reproducer")
    rust_plan = plan["rust_lost_update"]
    rust_seeded = tuple(
        (int(rust_plan["seed"]) >> shift) & 1
        for shift in range(int(rust_plan["max_choices"]))
    )
    if rust_seeded != tuple(
        int(value) for value in by_id["rust_lost_update"]["reproducer_schedule"]
    ):
        raise ValueError("seeded Rust reproducer does not derive from its recorded seed")
    normalized_plan = "".join(plan_text.split())
    normalized_searcher = "".join(args.searcher_source.read_text().split())
    discovered = tuple(int(value) for value in by_id["go_publish_before_init"]["reproducer_schedule"])
    literal = "[" + ",".join(str(value) for value in discovered) + "]"
    if literal in normalized_plan or literal in normalized_searcher:
        raise ValueError("held-out reproducer is present in a seed/fixture input")
    go_result = by_id["go_publish_before_init"]
    if int(go_result["attempts"]) > budget or int(go_result["budget"]) != budget:
        raise ValueError("held-out discovery exceeded or changed its predeclared budget")
    print(
        f"M6_WITHHELD_OK id=go_publish_before_init budget={budget} "
        f"first_seed_candidate_negative=true fixture_schedule_absent=true"
    )

    if args.plant_schedule:
        by_id[args.plant_schedule]["reproducer_schedule"] = [0, 0, 0]
    verify_bug(by_id["rust_lost_update"], run_rust, max_choices)
    verify_bug(by_id["go_publish_before_init"], run_go, max_choices)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
        print(f"M6_INDEPENDENT_FAIL {error}", file=sys.stderr)
        raise SystemExit(1)
