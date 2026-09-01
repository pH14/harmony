#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Independent semantic oracle for the M6 concurrency measurement."""

from __future__ import annotations

import argparse
import dataclasses
import itertools
import json
import pathlib
import sys
from typing import Callable

BOOTSTRAP_THREAD = (1 << 32) - 1


@dataclasses.dataclass(frozen=True)
class ModelOutcome:
    bug: bool
    value: int
    choices: tuple[int, ...]
    coverage: tuple[tuple[int, int, int, int], ...]


def choose(
    schedule: tuple[int, ...], cursor: int, ready: list[int]
) -> tuple[int, int, int]:
    if len(ready) == 1:
        return ready[0], cursor, 0
    if cursor >= len(schedule):
        raise ValueError("schedule exhausted")
    selected = schedule[cursor]
    if selected < 0 or selected >= len(ready):
        raise ValueError("selection outside runnable set")
    return ready[selected], cursor + 1, selected


def run_rust(schedule: tuple[int, ...]) -> ModelOutcome:
    steps = [0, 0]
    observed = [0, 0]
    local = [0, 0]
    shared = 0
    cursor = 0
    actor, cursor, selected = choose(schedule, cursor, [0, 1])
    choices = [selected]
    coverage = [(BOOTSTRAP_THREAD, 1, 2, selected)]
    while True:
        if steps[actor] == 0:
            local[actor] = shared
        else:
            shared = local[actor] + 1
        steps[actor] += 1
        observed[actor] += 1
        ready = [index for index, step in enumerate(steps) if step < 2]
        if not ready:
            return ModelOutcome(
                shared != 2,
                shared,
                tuple(choices),
                tuple(coverage),
            )
        yielding_actor = actor
        actor, cursor, selected = choose(schedule, cursor, ready)
        if len(ready) > 1:
            choices.append(selected)
        coverage.append(
            (yielding_actor, observed[yielding_actor], len(ready), selected)
        )


def run_go(schedule: tuple[int, ...]) -> ModelOutcome:
    steps = [0, 0]
    per_actor_observed = [0, 0]
    published = False
    initialized = 0
    saw_published = False
    observed = 0xFF
    cursor = 0
    actor, cursor, selected = choose(schedule, cursor, [0, 1])
    choices = [selected]
    coverage = [(BOOTSTRAP_THREAD, 1, 2, selected)]
    while True:
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
        per_actor_observed[actor] += 1
        ready = [index for index, step in enumerate(steps) if step < 2]
        if not ready:
            return ModelOutcome(
                saw_published and observed == 0,
                observed,
                tuple(choices),
                tuple(coverage),
            )
        yielding_actor = actor
        actor, cursor, selected = choose(schedule, cursor, ready)
        if len(ready) > 1:
            choices.append(selected)
        coverage.append(
            (
                yielding_actor,
                per_actor_observed[yielding_actor],
                len(ready),
                selected,
            )
        )


def schedules(max_choices: int) -> list[tuple[int, ...]]:
    return list(itertools.product((0, 1), repeat=max_choices))


def verify_bug(
    result: dict[str, object],
    model: Callable[[tuple[int, ...]], ModelOutcome],
    max_choices: int,
) -> None:
    bug_id = str(result["id"])
    wrong = tuple(int(value) for value in result["wrong_schedule"])
    reproducer = tuple(int(value) for value in result["reproducer_schedule"])
    wrong_outcome = model(wrong)
    if wrong_outcome.bug or bool(result["wrong_schedule_reproduced"]):
        raise ValueError(f"{bug_id}: wrong-schedule negative did not hold")
    outcome = model(reproducer)
    if not outcome.bug or outcome.value != int(result["result_value"]):
        raise ValueError(f"{bug_id}: reported schedule is not an independent reproducer")
    if wrong_outcome.value != int(result["wrong_result_value"]):
        raise ValueError(f"{bug_id}: wrong-schedule value differs from the independent model")
    if outcome.choices != reproducer or wrong_outcome.choices != wrong:
        raise ValueError(f"{bug_id}: report contains unused schedule choices")
    known = [candidate for candidate in schedules(max_choices) if model(candidate).bug]
    if not known:
        raise ValueError(f"{bug_id}: independent enumeration found no reproducer")
    if not bool(result["deterministic_replay"]):
        raise ValueError(f"{bug_id}: deterministic replay was not recorded")
    for field in ("transcript_sha256", "wrong_transcript_sha256"):
        digest = str(result[field])
        if len(digest) != 64 or any(
            char not in "0123456789abcdef" for char in digest
        ):
            raise ValueError(f"{bug_id}: malformed {field}")
    coverage = result["coverage"]
    if len(coverage) != int(result["coverage_exits"]) or not coverage:
        raise ValueError(f"{bug_id}: coverage exit report is incomplete")
    reported_coverage = tuple(
        (
            int(record["thread"]),
            int(record["observed"]),
            int(record["ready"]),
            int(record["selected"]),
        )
        for record in coverage
    )
    if reported_coverage != outcome.coverage:
        raise ValueError(f"{bug_id}: coverage trace differs from the independent model")
    wrong_coverage = result["wrong_coverage"]
    if len(wrong_coverage) != int(result["wrong_coverage_exits"]) or not wrong_coverage:
        raise ValueError(f"{bug_id}: wrong-schedule coverage report is incomplete")
    reported_wrong_coverage = tuple(
        (
            int(record["thread"]),
            int(record["observed"]),
            int(record["ready"]),
            int(record["selected"]),
        )
        for record in wrong_coverage
    )
    if reported_wrong_coverage != wrong_outcome.coverage:
        raise ValueError(
            f"{bug_id}: wrong-schedule coverage differs from the independent model"
        )
    print(
        f"M6_INDEPENDENT_OK id={bug_id} "
        f"known_reproducers={len(known)} schedule={list(reproducer)}"
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("report", type=pathlib.Path)
    parser.add_argument("plan", type=pathlib.Path)
    parser.add_argument("searcher_source", type=pathlib.Path)
    parser.add_argument(
        "--plant-schedule",
        choices=("rust_lost_update", "go_publish_before_init"),
    )
    parser.add_argument("--plant-held-out-fixture", action="store_true")
    args = parser.parse_args()

    report = json.loads(args.report.read_text())
    plan_text = args.plan.read_text()
    plan = json.loads(plan_text)
    if report.get("format") != "consonance.m6-concurrency.v1":
        raise ValueError("unexpected report format")
    if plan.get("format") != "consonance.m6-plan.v1":
        raise ValueError("unexpected plan format")
    if report.get("held_out_seed") != plan["go_publish_before_init"]["seed"]:
        raise ValueError("report does not carry the predeclared held-out seed")
    by_id = {entry["id"]: entry for entry in report["bugs"]}
    if set(by_id) != {"rust_lost_update", "go_publish_before_init"}:
        raise ValueError("report is not per-bug complete")
    go_plan = plan["go_publish_before_init"]
    if "schedule" in go_plan:
        raise ValueError("held-out plan contains a reproducing schedule fixture")
    budget = int(go_plan["budget"])
    seed = int(go_plan["seed"])
    max_choices = int(go_plan["max_choices"])
    if budget != 1 << max_choices:
        raise ValueError("held-out budget must enumerate the declared schedule vocabulary")
    first = tuple((seed >> shift) & 1 for shift in range(max_choices))
    if run_go(first).bug:
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
    discovered = tuple(
        int(value)
        for value in by_id["go_publish_before_init"]["reproducer_schedule"]
    )
    literal = "[" + ",".join(str(value) for value in discovered) + "]"
    if args.plant_held_out_fixture:
        normalized_searcher += literal
    if literal in normalized_plan or literal in normalized_searcher:
        raise ValueError("held-out reproducer is present in a seed/fixture input")
    go_result = by_id["go_publish_before_init"]
    expected_attempt = None
    expected_schedule = None
    for attempt in range(budget):
        word = attempt ^ (seed & (budget - 1))
        candidate = tuple((word >> shift) & 1 for shift in range(max_choices))
        candidate_outcome = run_go(candidate)
        if candidate_outcome.bug:
            expected_attempt = attempt + 1
            expected_schedule = candidate_outcome.choices
            break
    if expected_attempt is None or expected_schedule is None:
        raise ValueError("independent search found no held-out reproducer in budget")
    if (
        int(go_result["attempts"]) != expected_attempt
        or int(go_result["budget"]) != budget
        or discovered != expected_schedule
    ):
        raise ValueError("held-out discovery exceeded or changed its predeclared budget")
    expected_fields = {
        "rust_lost_update": ("rust", "seeded_reproducer", int(rust_plan["seed"]), 1),
        "go_publish_before_init": ("go", "held_out_discovery", seed, budget),
    }
    for bug_id, (language, mode, recorded_seed, recorded_budget) in expected_fields.items():
        result = by_id[bug_id]
        if (
            result.get("language") != language
            or result.get("mode") != mode
            or int(result["seed"]) != recorded_seed
            or int(result["budget"]) != recorded_budget
        ):
            raise ValueError(f"{bug_id}: report metadata differs from the predeclared plan")
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
