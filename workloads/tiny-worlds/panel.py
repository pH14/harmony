#!/usr/bin/env -S uv run --script
# SPDX-License-Identifier: AGPL-3.0-or-later
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Run the tiny-worlds searcher panel and check each rule against its expected direction."""
from __future__ import annotations

import argparse
import collections
import concurrent.futures
import json
import secrets
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
BUDGET = 20_000


def pattern(length: int) -> int:
    while True:
        value = secrets.randbits(2 * length)
        if value & 3 != 3:
            return value


def delayed(placement: str, ammo: int = 0, sticky: bool = False) -> dict:
    return {"family": "delayed", "parameters": {
        "horizon": 16, "distractions": 32, "mode": "sequence",
        "placement": placement, "sticky_credit": sticky, "ammo": ammo}}


def chain(ranked: bool, ranked_upgrade: bool, placement: str, sticky: bool, patterns) -> dict:
    stages = []
    for route_pattern, trap_pattern in patterns:
        stages += [
            {"world": {"family": "resource", "parameters": {
                "initial_charge": 0, "initial_health": 3, "barrier_charge": 12, "route_cost": 1,
                "health_cost": 0, "refill_amount": 1, "max_charge": 20, "corridor_len": 6,
                "refill_health_cost": 0}}, "refill_available": True},
            {"world": {"family": "route", "parameters": {
                "length": 16, "pattern": route_pattern, "attack": 2, "shifted": True,
                "upgrade_required": True, "ranked_upgrade": ranked_upgrade}}, "refill_available": True},
            {"world": delayed(placement, 0, sticky), "refill_available": True},
            {"world": {"family": "trap", "parameters": {
                "length": 12, "pattern": trap_pattern, "trap_len": 1, "rooms": 16}}, "refill_available": True},
        ]
    return {"family": "chain", "parameters": {
        "stages": stages, "carry_charge": True, "initial_charge": 0, "ranked": ranked}}


def request(arm: str, config: dict, seed: int, broken: bool = False, keep: str = "portfolio") -> dict:
    return {"arm": arm, "request": {"config": config, "seed": seed, "work_budget": BUDGET,
                                    "broken": broken, "verify": True, "keep": keep}}


def requests(seeds: int) -> list[dict]:
    rows = []
    for _ in range(2 * seeds):
        seed = secrets.randbits(64)
        rows.append(request("boss/engaged/tight", delayed("engaged", 16), seed))
        rows.append(request("boss/tier/tight", delayed("tier", 16), seed))
    for _ in range(seeds):
        seed = secrets.randbits(64)
        rows += [
            request("boss/place/tight", delayed("place", 16), seed),
            request("boss/engaged/ample", delayed("engaged", 31), seed),
            request("boss/tier/ample", delayed("tier", 31), seed),
            request("credit/engaged", delayed("engaged"), seed),
            request("credit/engaged/sticky", delayed("engaged", 0, True), seed),
        ]
        trap = {"family": "trap", "parameters": {"length": 12, "pattern": pattern(12), "trap_len": 2, "rooms": 1}}
        rows.append(request("trap/ranked", trap, seed))
        rows.append(request("trap/control", trap, seed, broken=True))
        stages = [(pattern(16), pattern(12)) for _ in range(4)]
        rows += [
            request("chain/flat", chain(False, False, "place", False, stages), seed),
            request("chain/ranked", chain(True, False, "place", False, stages), seed),
            request("chain/ranked_upgrade", chain(True, True, "engaged", False, stages), seed),
            request("chain/sticky", chain(True, True, "engaged", True, stages), seed),
        ]
        short = chain(True, True, "engaged", False, [(pattern(16), pattern(12)) for _ in range(2)])
        rows.append(request("keep/portfolio", short, seed))
        rows.append(request("keep/capacity_two", short, seed, keep="capacity_two"))
        line = pattern(32)
        for arm, placement, broken in (("tier", "tier", False), ("identity", "identity", False),
                                       ("preference", "preference", False), ("control", "tier", True)):
            rows.append(request(f"backtrack/{arm}", {"family": "backtrack", "parameters": {
                "barriers": 3, "segment": 8, "pattern": line, "placement": placement}}, seed, broken))
        grid = {"family": "map", "parameters": {"width": 8, "height": 8, "layout": secrets.randbits(64),
                                                "loops": 7, "corridor": 2, "shaft": 3, "inner": 20}}
        rows.append(request("map/ranked", grid, seed))
    return rows


def execute(binary: Path, job: dict) -> dict:
    process = subprocess.run([str(binary)], input=json.dumps(job["request"]),
                             capture_output=True, text=True)
    if process.returncode:
        raise RuntimeError(f"{job['arm']} seed {job['request']['seed']}: {process.stderr.strip()[-400:]}")
    report = json.loads(process.stdout)
    report["arm"] = job["arm"]
    return report


def evaluate(rows: list[dict]) -> list[tuple[str, str, bool]]:
    by = collections.defaultdict(list)
    for row in rows:
        by[row["arm"]].append(row)

    def solved(arm):
        return sum(r["success"] for r in by[arm])

    def median(arm, unsolved):
        return statistics.median(r["first_objective_work"] if r["success"] else unsolved for r in by[arm])

    def low(arm):
        return median(arm, BUDGET)

    def high(arm):
        return median(arm, float("inf"))

    def shown(arm):
        return f"{low(arm):.0f}" if low(arm) == high(arm) else f"{low(arm):.0f}+"

    def slow(arm):
        return sum(not r["success"] or r["first_objective_work"] > 1000 for r in by[arm])

    def tier_draws(r):
        return [d for d in r["parent_draws"] if d[0] and d[1] == "tiers"]

    def map_return_trip(arm):
        ratios = []
        for r in by[arm]:
            entry, item, out, _ = (w if w is not None and w <= BUDGET else None
                                   for w in (r["evidence"]["map_first"] + [None] * 4)[:4])
            first_trip = item - entry if item is not None else BUDGET
            ratios.append((out - item if out is not None else float("inf")) / first_trip)
        return statistics.median(ratios)

    def trap_share(arm):
        return statistics.median(
            sum(d[5] for d in tier_draws(r) if d[3] == 1) / max(1, sum(d[5] for d in tier_draws(r)))
            for r in by[arm])

    def top_share(arm):
        top = lower = 0
        for r in by[arm]:
            top += sum(d[5] for d in tier_draws(r) if d[3] == 1)
            lower += sum(d[5] for d in tier_draws(r) if d[3] == 0 and d[2] == 1)
            lower += sum(d[3] for d in r["skipped_draws"] if d[0] and d[1] == "tiers" and d[2] == 1)
        return top / max(1, top + lower)

    n = len(by["credit/engaged"])
    m = len(by["boss/tier/tight"])
    third = n // 3
    most = -(-2 * n // 3)
    return [
        ("boss, tight ammo: engaged runs over 1,000 work or unsolved", f"{slow('boss/engaged/tight')}/{m}",
         slow("boss/engaged/tight") <= -(-m // 12)),
        ("boss, tight ammo: level-as-tier runs over 1,000 work or unsolved", f"{slow('boss/tier/tight')}/{m}",
         slow("boss/tier/tight") >= -(-m // 3)),
        ("boss, tight ammo: place slower than engaged", f"{shown('boss/place/tight')} vs {shown('boss/engaged/tight')}",
         low("boss/place/tight") > high("boss/engaged/tight")),
        ("boss, ample ammo: level-as-tier faster than engaged", f"{shown('boss/tier/ample')} vs {shown('boss/engaged/ample')}",
         high("boss/tier/ample") < low("boss/engaged/ample")),
        ("credit kept after leaving: over 4x slower", f"{shown('credit/engaged/sticky')} vs {shown('credit/engaged')}",
         low("credit/engaged/sticky") > 4 * high("credit/engaged")),
        ("trap: ranked item over 3x slower than control", f"{shown('trap/ranked')} vs {shown('trap/control')}",
         low("trap/ranked") > 3 * high("trap/control")),
        ("trap: item draw share at least 0.8 ranked, at most 0.5 control", f"{trap_share('trap/ranked'):.2f} vs {trap_share('trap/control'):.2f}",
         trap_share("trap/ranked") >= 0.8 and trap_share("trap/control") <= 0.5),
        ("trap: top-tier draw share 0.87-0.91", f"{top_share('trap/ranked'):.3f}",
         0.87 <= top_share("trap/ranked") <= 0.91),
        ("keep: capacity 2 / portfolio median 0.67-1.5", f"{shown('keep/capacity_two')} vs {shown('keep/portfolio')}",
         0.67 <= low("keep/capacity_two") / high("keep/portfolio") and high("keep/capacity_two") / low("keep/portfolio") <= 1.5),
        ("backtrack: ranked items slower than preference", f"{shown('backtrack/tier')} vs {shown('backtrack/preference')}",
         low("backtrack/tier") > high("backtrack/preference")),
        ("backtrack: identity slowest", f"{shown('backtrack/identity')} vs {shown('backtrack/tier')}",
         low("backtrack/identity") > high("backtrack/tier")),
        ("backtrack: control mostly unsolved", f"{solved('backtrack/control')}/{n}", solved("backtrack/control") <= third),
        ("chain: flat mostly unsolved", f"{solved('chain/flat')}/{n}", solved("chain/flat") <= third),
        ("chain: ranked stages solve", f"{solved('chain/ranked')}/{n}", solved("chain/ranked") >= most),
        ("chain: ranked stages and upgrade solve", f"{solved('chain/ranked_upgrade')}/{n}", solved("chain/ranked_upgrade") >= most),
        ("chain: kept boss credit mostly unsolved", f"{solved('chain/sticky')}/{n}", solved("chain/sticky") <= third),
        ("map: return trip shorter than the first trip", f"{map_return_trip('map/ranked'):.2f}",
         map_return_trip("map/ranked") < 1),
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seeds", type=int, default=6, help="runtime seeds per arm")
    parser.add_argument("--jobs", type=int, default=6, help="parallel processes")
    parser.add_argument("--binary", type=Path, help="prebuilt tiny-worlds executable")
    args = parser.parse_args()
    if args.seeds < 3:
        parser.error("--seeds must be at least 3")
    binary = args.binary
    if binary is None:
        subprocess.run(["cargo", "build", "--release", "--locked", "--manifest-path",
                        str(ROOT / "Cargo.toml")], check=True)
        binary = ROOT / "target" / "release" / "tiny-worlds"
    jobs = requests(args.seeds)
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        rows = list(pool.map(lambda job: execute(binary, job), jobs))
    if not all(r["verified"] for r in rows):
        print("a run did not verify", file=sys.stderr)
        return 1
    failed = 0
    for name, value, ok in evaluate(rows):
        failed += not ok
        print(f"{'PASS' if ok else 'FAIL'}  {name}: {value}")
    print(f"{len(rows)} runs, {failed} failed rules")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
