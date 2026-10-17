#!/usr/bin/env -S uv run --script
# SPDX-License-Identifier: AGPL-3.0-or-later
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Compare searcher builds on the calibrated stall worlds with Kaplan-Meier curves and log-rank tests."""
from __future__ import annotations

import argparse
import concurrent.futures
import json
import math
import secrets
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
GATED = {"items": 4, "leg": 10, "walk": 44, "barrier": 40, "step_run": 4, "fight": 4, "fight_odds": 4, "health": 8,
         "hit_odds": 8, "shortcut": 1, "shortcut_run": 3, "dense": False}
DEAD_END = {"length": 40, "fork": 24, "dead_len": 10, "rooms": 255, "room_run": 4, "entry_run": 2, "step_run": 2,
            "shared_rooms": 32, "grades": 15}
OFF_AXIS = {"length": 25, "fork": 24, "dead_len": 10, "rooms": 255, "room_run": 1, "entry_run": 2, "step_run": 2,
            "goal_at_fork": True, "run_on": 60_000, "run_on_run": 3}
WORLDS = {
    "resource-gated shortcut": ("gated", GATED, 2_000_000),
    "resource-gated, no shortcut": ("gated", {**GATED, "shortcut": 0, "shortcut_run": 0}, 2_000_000),
    "dense progress": ("gated", {**GATED, "dense": True}, 2_000_000),
    "dead end below the leading tier": ("fork", DEAD_END, 2_000_000),
    "goal off the progress axis": ("fork", OFF_AXIS, 1_000_000),
    "goal off the progress axis, no run-on": ("fork", {**OFF_AXIS, "run_on": 0, "run_on_run": 0}, 1_000_000),
}


def request(family: str, parameters: dict, seed: int, pattern: int, budget: int) -> dict:
    return {"config": {"family": family, "parameters": {**parameters, "pattern": pattern}}, "seed": seed,
            "work_budget": budget, "broken": False, "verify": False, "keep": "portfolio"}


def execute(binary: Path, job: dict) -> tuple[float, bool]:
    process = subprocess.run([str(binary)], input=json.dumps(job), capture_output=True, text=True, check=False)
    if process.returncode:
        raise RuntimeError(f"seed {job['seed']}: {process.stderr.strip()[-400:]}")
    report = json.loads(process.stdout)
    if report["success"]:
        return float(report["first_objective_execution"]), True
    return float(report["executions"]), False


def kaplan_meier(times: list[tuple[float, bool]]) -> list[tuple[float, float]]:
    curve, alive, survival = [(0.0, 1.0)], len(times), 1.0
    for t in sorted({t for t, event in times if event}):
        deaths = sum(1 for u, event in times if u == t and event)
        at_risk = sum(1 for u, _ in times if u >= t)
        survival *= 1 - deaths / at_risk
        curve.append((t, survival))
        alive -= deaths
    return curve


def quantile(curve: list[tuple[float, float]], q: float) -> float:
    for t, survival in curve:
        if survival <= 1 - q:
            return t
    return math.inf


def log_rank(a: list[tuple[float, bool]], b: list[tuple[float, bool]]) -> tuple[float, float]:
    observed_minus_expected, variance = 0.0, 0.0
    for t in sorted({t for t, event in a + b if event}):
        na = sum(1 for u, _ in a if u >= t)
        nb = sum(1 for u, _ in b if u >= t)
        da = sum(1 for u, event in a if u == t and event)
        db = sum(1 for u, event in b if u == t and event)
        n, d = na + nb, da + db
        if n < 2:
            continue
        observed_minus_expected += da - d * na / n
        variance += d * (na / n) * (nb / n) * (n - d) / (n - 1)
    if variance == 0:
        return 0.0, 1.0
    z = observed_minus_expected / math.sqrt(variance)
    return z, math.erfc(abs(z) / math.sqrt(2))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--arm", action="append", required=True, metavar="NAME=BINARY",
                        help="a tiny-worlds executable; the first arm is the baseline")
    parser.add_argument("--world", action="append", choices=list(WORLDS), help="limit to this world; repeatable")
    parser.add_argument("--seeds", type=int, default=20, help="runtime seeds and layouts, shared by every arm and world")
    parser.add_argument("--jobs", type=int, default=8, help="parallel processes")
    parser.add_argument("--curves", type=Path, help="write each arm's Kaplan-Meier curve to this JSON file")
    args = parser.parse_args()
    arms = [(name, Path(path).resolve()) for name, path in (a.split("=", 1) for a in args.arm)]
    curves = {}
    draws = [(secrets.randbits(63), secrets.randbits(64)) for _ in range(args.seeds)]
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        for world in args.world or list(WORLDS):
            family, parameters, budget = WORLDS[world]
            jobs = [request(family, parameters, seed, pattern, budget) for seed, pattern in draws]
            times = {name: list(pool.map(lambda job, b=binary: execute(b, job), jobs)) for name, binary in arms}
            base = arms[0][0]
            print(f"{world}: {args.seeds} paired seeds, tries to the goal, budget {budget:,} work")
            for name, _ in arms:
                curve = kaplan_meier(times[name])
                curves.setdefault(world, {})[name] = {"curve": curve, "runs": times[name]}
                solved = sum(event for _, event in times[name])
                line = (f"    {name:10} solved {solved}/{args.seeds}  quartiles {quantile(curve, 0.25):,.0f} / "
                        f"{quantile(curve, 0.5):,.0f} / {quantile(curve, 0.75):,.0f}")
                if name != base:
                    z, p = log_rank(times[name], times[base])
                    line += f"  log-rank vs {base}: z {z:+.2f} p {p:.3f} ({'more' if z > 0 else 'fewer'} finishes than expected)"
                print(line)
    if args.curves:
        args.curves.write_text(json.dumps(curves))
    return 0


if __name__ == "__main__":
    sys.exit(main())
