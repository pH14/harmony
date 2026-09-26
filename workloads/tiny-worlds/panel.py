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
import math
import random
import secrets
import statistics
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent
BUDGET = 20_000
WORLD_BUDGET = 200_000
SLOWER, FASTER = 1.25, 0.8
WORLDS = {
    "farm loop": ({"inner": 20, "farms": 4, "farm_cap": 63}, 1),
    "whole-map re-walk": ({"inner": 4, "items": 9}, 1),
    "boss needing far stock": ({"inner": 20, "farms": 4, "farm_cap": 63, "boss_stock": 24}, 1),
    "boss needing stock and health": ({"inner": 20, "farms": 4, "farm_cap": 63, "boss_stock": 6,
                                       "boss_hits_back": True}, 1),
    "off-path item": ({"inner": 6, "item_optional": True}, 4),
    "off-path item with farms": ({"inner": 6, "item_optional": True, "farms": 2, "farm_cap": 63}, 4),
    "locked item": ({"inner": 4, "locked": True}, 4),
    "locked item with hidden timing": ({"inner": 4, "locked": True, "timing": 5}, 4),
}


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
        for arm, broken in (("ranked", False), ("control", True)):
            rows.append(request(f"map/{arm}", grid, seed, broken))
    return rows


def world_requests(scale: int) -> list[dict]:
    rows = []
    for world, (fields, multiplier) in WORLDS.items():
        for _ in range(multiplier * scale):
            config = {"family": "map", "parameters": {
                "width": 8, "height": 8, "layout": secrets.randbits(64), "loops": 7, "corridor": 2,
                "shaft": 3, **fields}}
            rows.append({"arm": world, "request": {"config": config, "seed": secrets.randbits(64),
                                                   "work_budget": WORLD_BUDGET, "broken": False,
                                                   "verify": False, "keep": "portfolio"}})
    return rows


def legs(report: dict) -> dict:
    evidence = report["evidence"]

    def within(work):
        return work if work is not None and work <= WORLD_BUDGET else None

    goal = within(report["first_objective_work"])
    first = [within(w) for w in evidence["map_first"]]
    tiers = [within(w) for w in evidence["map_first_tier"]]

    def gap(start, end):
        return None if start is None or end is None else end - start

    measured = {"to the goal": WORLD_BUDGET if goal is None else goal}
    parameters = report["config"]["parameters"]
    if parameters.get("boss_stock"):
        measured["to the item"] = tiers[1]
        measured["item to stocked arrival"] = gap(tiers[1], within(evidence["map_first_stocked"]))
        measured["stocked arrival to kill"] = gap(within(evidence["map_first_stocked"]), goal)
    elif parameters.get("locked"):
        measured["to the key"] = tiers[1]
        measured["key to the item"] = gap(tiers[1], tiers[2])
        measured["item to the goal"] = gap(tiers[2], goal)
    elif parameters.get("items", 1) > 1:
        measured["to the last item"] = tiers[-1]
        measured["last item to the goal"] = gap(tiers[-1], goal)
    elif parameters.get("item_optional"):
        measured["pickup to the goal"] = gap(tiers[1], goal)
    else:
        measured["to the item"] = tiers[1]
        measured["out of the item region"] = gap(first[1], first[2])
        measured["out to the goal"] = gap(first[2], goal)
    return measured


def paired(base: list[dict], candidate: list[dict]) -> list[tuple[str, str, float, float, float]]:
    rand = random.Random(0)
    base_legs, candidate_legs = list(map(legs, base)), list(map(legs, candidate))
    rows = []
    for leg in base_legs[0]:
        reached = (f"reached {sum(c[leg] is not None for c in candidate_legs)}"
                   f" vs {sum(b[leg] is not None for b in base_legs)}")
        ratios = [math.log((c[leg] + 1) / (b[leg] + 1)) for b, c in zip(base_legs, candidate_legs)
                  if b[leg] is not None and c[leg] is not None]
        if len(ratios) < 3:
            rows.append((leg, reached, math.nan, math.nan, math.nan))
            continue
        draws = sorted(statistics.median(rand.choices(ratios, k=len(ratios))) for _ in range(2000))
        rows.append((leg, reached, math.exp(statistics.median(ratios)),
                     math.exp(draws[10]), math.exp(draws[1989])))
    return rows


def verdict(low: float, high: float) -> str:
    if low > SLOWER:
        return "slower"
    if high < FASTER:
        return "faster"
    return "no clear change"


def compare(baseline: Path, candidate: Path, scale: int, jobs: int) -> int:
    runs = world_requests(scale)
    with concurrent.futures.ThreadPoolExecutor(jobs) as pool:
        base = list(pool.map(lambda job: execute(baseline, job), runs))
        cand = list(pool.map(lambda job: execute(candidate, job), runs))
    bad = []
    for world in WORLDS:
        b = [r for r in base if r["arm"] == world]
        c = [r for r in cand if r["arm"] == world]
        missed = (sum(not r["success"] for r in b), sum(not r["success"] for r in c))
        identical = sum(x["stream_sha256"] == y["stream_sha256"] for x, y in zip(b, c))
        results = paired(b, c)
        slower = [leg for leg, _, _, low, high in results if verdict(low, high) == "slower"]
        clearly_bad = bool(slower) or missed[1] > missed[0] + 1
        if clearly_bad:
            bad.append(world)
        print(f"{world}: {'CLEARLY BAD' if clearly_bad else 'plausible'}; "
              f"goal missed {missed[1]}/{len(c)} vs {missed[0]}/{len(b)}; identical runs {identical}/{len(b)}")
        for leg, reached, ratio, low, high in results:
            print(f"    {leg:26} {ratio:5.2f}x  [{low:.2f}, {high:.2f}]  {reached}  {verdict(low, high)}")
    print(f"{len(base) + len(cand)} world runs; clearly bad on {len(bad)} of {len(WORLDS)} worlds")
    return 1 if bad else 0


def execute(binary: Path, job: dict) -> dict:
    process = subprocess.run([str(binary)], input=json.dumps(job["request"]),
                             capture_output=True, text=True, check=False)
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

    def map_ratios(arm):
        return_trip, next_gap = [], []
        for r in by[arm]:
            entry, item, out, goal = (w if w is not None and w <= BUDGET else None
                                      for w in (r["evidence"]["map_first"] + [None] * 4)[:4])
            first_trip = item - entry if item is not None else BUDGET
            back = out - item if out is not None else float("inf")
            return_trip.append(back / first_trip)
            if out is None:
                next_gap.append(0)
            else:
                rooms = r["layout"]["door_to_item"] / r["layout"]["door_to_goal"]
                next_gap.append(((goal if goal is not None else BUDGET) - out) / back * rooms)
        return statistics.median(return_trip), statistics.median(next_gap)

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
        ("map: return trip shorter than the first trip", f"{map_ratios('map/ranked')[0]:.2f}",
         map_ratios("map/ranked")[0] < 1),
        ("map: gap after leaving longer than the return trip per room", f"{map_ratios('map/ranked')[1]:.2f}",
         map_ratios("map/ranked")[1] > 1),
        ("map: hidden item mostly unsolved", f"{solved('map/control')}/{n}", solved("map/control") <= third),
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seeds", type=int, default=6, help="runtime seeds per arm")
    parser.add_argument("--jobs", type=int, default=6, help="parallel processes")
    parser.add_argument("--binary", type=Path, help="prebuilt tiny-worlds executable")
    parser.add_argument("--compare", type=Path, metavar="BASELINE",
                        help="also run the Metroid worlds on this baseline executable and on --binary")
    parser.add_argument("--world-scale", type=int, default=16,
                        help="layouts per heavy Metroid world; light worlds run four times as many")
    args = parser.parse_args()
    if args.seeds < 3:
        parser.error("--seeds must be at least 3")
    if args.world_scale < 3:
        parser.error("--world-scale must be at least 3")
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
    if args.compare:
        failed += compare(args.compare, binary, args.world_scale, args.jobs)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
