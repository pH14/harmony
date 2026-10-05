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
MISS_BAND = 0.05
MAX_WORLD_SCALE = 256
REPORT_FIELDS = {"config", "crossing", "evidence", "first_objective_execution", "first_objective_work", "layout",
                 "scale", "stream_sha256", "success", "verified", "work_budget"}
SHIELDED_BOSS = {"inner": 20, "farms": 4, "farm_cap": 63, "boss_stock": 24, "shield": 8, "shield_odds": 4,
                 "hit_tier": True, "tail_slots": True}
WORLDS = {
    "flat archive crossing": ({"cells": 1024, "length": 4, "rooted": False}, 1),
    "fresh crossing": ({"cells": 1024, "length": 4, "rooted": True}, 1),
    "rare flat archive crossing": ({"cells": 256, "length": 8, "rooted": False,
                                    "action_denominator": 1024}, 1),
    "rare fresh crossing": ({"cells": 256, "length": 8, "rooted": True,
                             "action_denominator": 1024}, 1),
    "passive clock hidden gap": ({"passive_clock": "hidden"}, 1),
    "passive clock visible progress": ({"passive_clock": "visible"}, 1),
    "farm loop": ({"inner": 20, "farms": 4, "farm_cap": 63}, 1),
    "whole-map re-walk": ({"inner": 4, "items": 9}, 1),
    "boss needing far stock": ({"inner": 20, "farms": 4, "farm_cap": 63, "boss_stock": 24}, 1),
    "boss needing stock and health": ({"inner": 20, "farms": 4, "farm_cap": 63, "boss_stock": 6,
                                       "boss_hits_back": True}, 1),
    "boss after a draining approach": ({"inner": 20, "farms": 4, "farm_cap": 14, "boss_stock": 8,
                                        "boss_hits_back": True, "approach_drain": True}, 1),
    "off-path item": ({"inner": 6, "item_optional": True}, 4),
    "off-path item with farms": ({"inner": 6, "item_optional": True, "farms": 2, "farm_cap": 63}, 4),
    "locked item": ({"inner": 4, "locked": True}, 4),
    "locked item with hidden timing": ({"inner": 4, "locked": True, "timing": 5}, 4),
    "gauntlet": ({"inner": 20, "items": 2, "farms": 2, "farm_cap": 1, "gauntlet": True}, 1),
    "gauntlet with hidden timing": ({"inner": 20, "items": 2, "farms": 2, "farm_cap": 1, "gauntlet": True,
                                     "timing": 5}, 1),
    "boss by the door": ({"inner": 2, "farms": 4, "farm_cap": 14, "boss_stock": 8, "boss_hits_back": True,
                          "approach_drain": True, "boss_by_door": True, "tail_slots": True}, 1),
    "boss beside a late item": ({"inner": 20, "farms": 4, "farm_cap": 63, "boss_stock": 8, "boss_hits_back": True,
                                 "late_item": True}, 1),
    "boss past an item at the entry": ({"inner": 20, "farms": 4, "farm_cap": 63, "boss_stock": 8,
                                        "boss_hits_back": True, "late_item": True, "item_at_entry": True,
                                        "tail_slots": True}, 1),
    "boss behind a shield with damage as a tier": (SHIELDED_BOSS, 1),
}
BUDGETS = {"boss by the door": 600_000, "boss beside a late item": 600_000, "boss past an item at the entry": 600_000,
           "boss behind a shield with damage as a tier": 600_000}


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


def request(arm: str, config: dict, seed: int, broken: bool = False, keep: str = "portfolio",
            budget: int = BUDGET) -> dict:
    return {"arm": arm, "request": {"config": config, "seed": seed, "work_budget": budget,
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
        for _ in range(2):
            shielded = {"family": "map", "parameters": {"width": 8, "height": 8, "layout": secrets.randbits(64),
                                                        "loops": 7, "corridor": 2, "shaft": 3, **SHIELDED_BOSS}}
            hidden = {**shielded, "parameters": {**shielded["parameters"], "hit_tier": False}}
            shield_seed = secrets.randbits(64)
            rows.append(request("shield/tier", shielded, shield_seed, budget=600_000))
            rows.append(request("shield/hidden", hidden, shield_seed, budget=600_000))
        layout = secrets.randbits(64)
        for arm, farms in (("farms", 4), ("none", 0)):
            rows.append(request(f"farm/{arm}", {"family": "map", "parameters": {
                "width": 8, "height": 8, "layout": layout, "loops": 7, "corridor": 2, "shaft": 3, "inner": 20,
                "farms": farms, "farm_cap": 63 if farms else 0}}, seed, budget=WORLD_BUDGET))
    return rows


def world_requests(world: str, count: int) -> list[dict]:
    fields, multiplier = WORLDS[world]
    rows = []
    for _ in range(multiplier * count):
        config = {"family": "map", "parameters": {
            "width": 8, "height": 8, "layout": secrets.randbits(64), "loops": 7, "corridor": 2,
            "shaft": 3, **fields}}
        if "cells" in fields:
            config = {"family": "crossing", "parameters": {
                **fields, "layout": secrets.randbits(64), "pattern": secrets.randbits(2 * fields["length"])}}
        search = None
        if "passive_clock" in fields:
            end = 15 + 960 + secrets.randbelow(271)
            events = [12, 13, end - 2, end]
            if fields["passive_clock"] == "visible":
                events = sorted(set(events + list(range(53, end, 40))))
            config = {"family": "passive_clock", "parameters": {
                "events": events, "holds": [
                    {"minimum": 2, "maximum": 7, "weight": 2},
                    {"minimum": 2, "maximum": 12, "weight": 11},
                    {"minimum": 48, "maximum": 120, "weight": 11}]}}
            search = {"mixture": "energy_splice:6"}
        row = {"arm": world, "request": {"config": config, "seed": secrets.randbits(64),
                                          "work_budget": BUDGETS.get(world, WORLD_BUDGET), "broken": False,
                                          "verify": False, "keep": "portfolio"}}
        if search is not None:
            row["request"]["search"] = search
        if "action_denominator" in fields:
            row["request"].update({
                "work_budget": 2_000_000,
                "search": {"mixture": "energy_splice:6", "stop_on_objective": True},
                "scale": {"workers": 1, "window": 2,
                          "memory_budget_mib": 8192, "archive_entries": 4096,
                          "action_cost_ns": 0, "action_sleep_ns": 0, "snapshot_bytes": 0},
            })
        rows.append(row)
    return rows


def legs(report: dict) -> tuple[dict, dict]:
    if report["config"]["family"] == "crossing" and report.get("scale") is not None:
        budget = report["work_budget"]
        goal = report["first_objective_work"]
        goal = goal if report["success"] and goal is not None and goal <= budget else None
        horizon = budget if goal is None else goal
        entry_work = report["crossing"]["first_entry_work"]
        entry = (report["crossing"]["first_entry_execution"]
                 if entry_work is not None and entry_work <= horizon else None)
        end = report["first_objective_execution"] if goal is not None else None
        return ({"to the goal (actions)": horizon, "to crossing entry (tries)": entry},
                {"crossing entry to goal (tries)": None if entry is None or end is None else end - entry})
    evidence = report["evidence"]
    budget = report["work_budget"]

    goal = report["first_objective_work"]
    goal = goal if goal is not None and goal <= budget else None

    def within(work):
        return work if work is not None and work <= (budget if goal is None else goal) else None

    if report["config"]["family"] == "crossing":
        entry = within(evidence["crossing_first_entry_work"])
        return ({"to the goal": budget if goal is None else goal, "to crossing entry": entry},
                {"crossing entry to goal": None if entry is None or goal is None else goal - entry})
    if report["config"]["family"] == "passive_clock":
        entry = within(evidence["passive_clock"]["first_event_work"][1])
        return ({"to the goal": budget if goal is None else goal, "to wait entry": entry},
                {"wait entry to goal": None if entry is None or goal is None else goal - entry})
    first = [within(w) for w in evidence["map_first"]]
    tiers = [within(w) for w in evidence["map_first_tier"]]

    def gap(start, end):
        return None if start is None or end is None else end - start

    parameters = report["config"]["parameters"]
    stocked = within(evidence.get("map_first_stocked"))
    if parameters.get("gauntlet"):
        milestones = {"to the last item": tiers[-1], "to the full-health arrival": stocked}
        diagnostics = {"last item to full-health arrival": gap(tiers[-1], stocked),
                       "full-health arrival to the goal": gap(stocked, goal)}
    elif parameters.get("hit_tier"):
        milestones = {"to the item": tiers[1], "to the stocked arrival": stocked, "to the first hit": tiers[2]}
        diagnostics = {"item to stocked arrival": gap(tiers[1], stocked),
                       "first hit to kill": gap(tiers[2], goal)}
    elif parameters.get("boss_stock"):
        milestones = {"to the item": tiers[1], "to the stocked arrival": stocked}
        diagnostics = {"item to stocked arrival": gap(tiers[1], stocked),
                       "stocked arrival to kill": gap(stocked, goal)}
    elif parameters.get("locked"):
        milestones = {"to the key": tiers[1], "to the item": tiers[2]}
        diagnostics = {"key to the item": gap(tiers[1], tiers[2]), "item to the goal": gap(tiers[2], goal)}
    elif parameters.get("items", 1) > 1:
        milestones = {"to the last item": tiers[-1]}
        diagnostics = {"last item to the goal": gap(tiers[-1], goal)}
    elif parameters.get("item_optional"):
        milestones = {"to the pickup": tiers[1]}
        diagnostics = {"pickup to the goal": gap(tiers[1], goal)}
    else:
        milestones = {"to the item": tiers[1], "out of the item region": first[2]}
        diagnostics = {"through the item region": gap(first[1], first[2]),
                       "out to the goal": gap(first[2], goal)}
    milestones["to the goal"] = budget if goal is None else goal
    return milestones, diagnostics


def paired(base: list[dict], candidate: list[dict], kind: int) -> list[tuple[str, str, float, float, float]]:
    rand = random.Random(0)
    base_legs = [legs(r)[kind] for r in base]
    candidate_legs = [legs(r)[kind] for r in candidate]
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
    if FASTER <= low and high <= SLOWER:
        return "inside the band"
    return "undecided"


def sign_test(more: int, fewer: int) -> float:
    n = more + fewer
    return sum(math.comb(n, k) for k in range(more, n + 1)) / 2 ** n


def missed_goals(base: list[dict], candidate: list[dict]) -> tuple[str, int, int]:
    arm_only = sum(b["success"] and not c["success"] for b, c in zip(base, candidate))
    base_only = sum(c["success"] and not b["success"] for b, c in zip(base, candidate))
    net, band = arm_only - base_only, MISS_BAND * len(base)
    if net >= band and sign_test(arm_only, base_only) < 0.01:
        return "more misses", arm_only, base_only
    if -net >= band and sign_test(base_only, arm_only) < 0.01:
        return "fewer misses", arm_only, base_only
    rand = random.Random(0)
    diffs = [int(b["success"]) - int(c["success"]) for b, c in zip(base, candidate)]
    draws = sorted(sum(rand.choices(diffs, k=len(diffs))) / len(diffs) for _ in range(2000))
    if -MISS_BAND < draws[10] and draws[1989] < MISS_BAND:
        return "inside the band", arm_only, base_only
    return "undecided", arm_only, base_only


def compare(baseline: Path, candidate: Path, scale: int, jobs: int, worlds: list[str]) -> int:
    runs = {world: ([], []) for world in worlds}
    done = dict.fromkeys(worlds, 0)
    target = dict.fromkeys(worlds, scale)
    results = {}
    with concurrent.futures.ThreadPoolExecutor(jobs) as pool:
        while open_worlds := [w for w in worlds if done[w] < target[w]]:
            batch = [job for w in open_worlds for job in world_requests(w, target[w] - done[w])]
            pairs = list(pool.map(lambda job: (execute(baseline, job), execute(candidate, job)), batch))
            for world in open_worlds:
                b, c = runs[world]
                for x, y in pairs:
                    if x["arm"] == world:
                        b.append(x)
                        c.append(y)
                done[world] = target[world]
                results[world] = paired(b, c, 0)
                if (target[world] < MAX_WORLD_SCALE
                        and (missed_goals(b, c)[0] == "undecided"
                             or any(verdict(low, high) == "undecided" for _, _, _, low, high in results[world]))):
                    target[world] = min(2 * target[world], MAX_WORLD_SCALE)
    failed = []
    for world in worlds:
        b, c = runs[world]
        missed = (sum(not r["success"] for r in b), sum(not r["success"] for r in c))
        misses, arm_only, base_only = missed_goals(b, c)
        if misses == "undecided":
            misses = ("undecided" if arm_only - base_only >= MISS_BAND * len(b)
                      and sign_test(arm_only, base_only) < 0.05 else "watch")
        identical = sum(x["stream_sha256"] == y["stream_sha256"] for x, y in zip(b, c))
        verdicts = []
        for _, _, ratio, low, high in results[world]:
            v = verdict(low, high)
            if v == "undecided" and high > SLOWER:
                v = "undecided" if low > 1.0 else "watch"
            verdicts.append(v)
        if "slower" in verdicts or misses == "more misses":
            status = "CLEARLY BAD"
        elif misses == "undecided" or any(v == "undecided" and high > SLOWER
                                          for v, (_, _, _, _, high) in zip(verdicts, results[world])):
            status = "UNDECIDED"
        else:
            status = "plausible"
        if status != "plausible":
            failed.append(world)
        diagnostics = paired(b, c, 1)
        diagnosed = [verdict(low, high) for _, _, _, low, high in diagnostics]
        watched = [leg for (leg, *_), v in zip(results[world], verdicts) if v == "watch"]
        watched += [leg for (leg, *_), v in zip(diagnostics, diagnosed) if v == "slower"]
        if misses == "watch":
            watched.append("goal misses")
        print(f"{world}: {status}; {len(b)} layouts; goal missed {missed[1]}/{len(c)} vs {missed[0]}/{len(b)} "
              f"(missed by one only: {arm_only} vs {base_only}, {misses}); "
              f"identical runs {identical}/{len(b)}" + (f"; watch {', '.join(watched)}" if watched else ""))
        for (leg, reached, ratio, low, high), v in zip(results[world], verdicts):
            print(f"    {leg:32} {ratio:5.2f}x  [{low:.2f}, {high:.2f}]  {reached}  {v}")
        for (leg, reached, ratio, low, high), v in zip(diagnostics, diagnosed):
            print(f"    {leg:32} {ratio:5.2f}x  [{low:.2f}, {high:.2f}]  {reached}  {v}, diagnostic")
    runs_total = sum(2 * len(b) for b, _ in runs.values())
    print(f"{runs_total} world runs; clearly bad or undecided on {len(failed)} of {len(worlds)} worlds")
    return 1 if failed else 0


WORKERS = 1


def execute(binary: Path, job: dict) -> dict:
    request = job["request"]
    if WORKERS != 1:
        if request.get("scale") is not None:
            request = {**request, "scale": {**request["scale"], "workers": WORKERS, "window": WORKERS}}
        else:
            request = {**request, "workers": WORKERS}
    process = subprocess.run([str(binary)], input=json.dumps(request),
                             capture_output=True, text=True, check=False)
    if process.returncode:
        raise RuntimeError(f"{job['arm']} seed {job['request']['seed']}: {process.stderr.strip()[-400:]}")
    report = {key: value for key, value in json.loads(process.stdout).items() if key in REPORT_FIELDS}
    report["evidence"] = {key: value for key, value in report.get("evidence", {}).items()
                          if key.startswith(("map_", "crossing_", "passive_clock"))}
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

    def trip_out(r):
        out = (r["evidence"]["map_first"] + [None] * 4)[2]
        goal = r["first_objective_work"] if r["success"] else WORLD_BUDGET
        return WORLD_BUDGET if out is None else goal - out

    def farm_cost():
        return statistics.median(trip_out(f) / max(1, trip_out(c)) for f, c in zip(by["farm/farms"], by["farm/none"]))

    def shield_losses():
        return sum(h["success"] and not t["success"] for t, h in zip(by["shield/tier"], by["shield/hidden"]))

    def shield_gains():
        return sum(t["success"] and not h["success"] for t, h in zip(by["shield/tier"], by["shield/hidden"]))

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
        ("trap: an unwinnable top tier costs under 4x the control's work", f"{shown('trap/ranked')} vs {shown('trap/control')}",
         high("trap/ranked") < 4 * low("trap/control")),
        ("trap: with the item hidden, the goal takes under 300 work", shown("trap/control"),
         high("trap/control") < 300),
        ("keep: capacity 2 / portfolio median 0.67-1.5", f"{shown('keep/capacity_two')} vs {shown('keep/portfolio')}",
         0.67 <= low("keep/capacity_two") / high("keep/portfolio") and high("keep/capacity_two") / low("keep/portfolio") <= 1.5),
        ("backtrack: ranked items under 2.5x the preference work", f"{shown('backtrack/tier')} vs {shown('backtrack/preference')}",
         high("backtrack/tier") < 2.5 * low("backtrack/preference")),
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
        ("farm loop: farms off the route slow the trip out under 10x", f"{farm_cost():.2f}", farm_cost() < 10),
        ("shield: damage as a tier loses more kills than it gains against hidden damage",
         f"{shield_losses()} vs {shield_gains()}", shield_losses() > shield_gains()),
    ]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--seeds", type=int, default=6, help="runtime seeds per arm")
    parser.add_argument("--jobs", type=int, default=6, help="parallel processes")
    parser.add_argument("--binary", type=lambda p: Path(p).resolve(), help="prebuilt tiny-worlds executable")
    parser.add_argument("--compare", type=lambda p: Path(p).resolve(), metavar="BASELINE",
                        help="also run the game-mechanism worlds on this baseline executable and on --binary")
    parser.add_argument("--world-scale", type=int, default=16,
                        help="starting layouts per comparison world; light map worlds run four times as many")
    parser.add_argument("--workers", type=int, default=1,
                        help="search workers per run; values above one also set the admission window")
    parser.add_argument("--world", action="append", choices=list(WORLDS),
                        help="compare only this world; repeat for several")
    args = parser.parse_args()
    if not 1 <= args.workers <= 64:
        parser.error("--workers must be 1..64")
    global WORKERS
    WORKERS = args.workers
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
        failed += compare(args.compare, binary, args.world_scale, args.jobs, args.world or list(WORLDS))
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
