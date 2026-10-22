#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Write a random fault schedule for `harmony debug run --actions`.

The schedule draws from the faults workload's vocabulary with the parameter
ranges of `sample_action` in workloads/faults/src/archive.rs. It takes no
feedback from earlier tests. The vocabulary string is the `action_vocabulary`
that a search of the same image records in its stream.
"""

import argparse
import json
import random
import sys

TICK_MS = 10
TICK_US = 10_000
MAX_SCALE = 10
QUANTUM_SCALES = 16
PARK_EDGE_EXPONENTS = 14
CLOCK_OFFSET_EXPONENTS = 19
CLOCK_RATE_EXPONENTS = 13
CLOCK_RATE_UNIT_PPM = 1_000
CLOCK_RATE_STOPPED_PPM = -1_000_000
HEAL_RESTART_TICKS = 100

NODE_KINDS = {"kill": "Kill", "pause": "Pause", "restart": "Restart"}
DISK_KINDS = {"power_loss": "PowerLoss", "disk_error": "DiskError", "disk_stall": "DiskStall"}
ALL_KINDS = {"kill_all": "KillAll", "power_loss_all": "PowerLossAll"}


def parse_list(text):
    return [int(item) for item in text.split(",") if item]


def parse_vocabulary(text):
    fields = dict(part.split("=", 1) for part in text.split(";") if "=" in part)
    return {
        "nodes": int(fields["nodes"]),
        "hooks": parse_list(fields.get("hooks", "")),
        "disks": parse_list(fields.get("disks", "")),
        "checksummed": parse_list(fields.get("checksummed", "")),
        "events": fields.get("events", "none") != "none",
        "clock": fields.get("clock", "none") != "none",
        "exclude": [item for item in fields.get("exclude", "").split(",") if item],
    }


def offered_kinds(vocab):
    several = vocab["nodes"] > 1
    disks = bool(vocab["disks"])
    offered = [
        ("kill", True),
        ("pause", True),
        ("restart", True),
        ("hook", bool(vocab["hooks"])),
        ("kill_all", several),
        ("power_loss", disks),
        ("disk_error", disks),
        ("disk_stall", disks),
        ("disk_corrupt", bool(vocab["checksummed"])),
        ("power_loss_all", disks and several),
        ("clock", vocab["clock"]),
        ("clock_all", vocab["clock"] and several),
        ("event_kill", vocab["events"]),
        ("event_park", vocab["events"]),
    ]
    return [kind for kind, ok in offered if ok and kind not in vocab["exclude"]]


def log_ticks(rng):
    return 1 << rng.randrange(MAX_SCALE + 1)


def clock_shift(rng):
    rate = rng.randrange(2) == 1
    negative = rng.randrange(2) == 1
    if rate:
        magnitude = CLOCK_RATE_UNIT_PPM << rng.randrange(CLOCK_RATE_EXPONENTS)
    else:
        magnitude = 1 << rng.randrange(CLOCK_OFFSET_EXPONENTS)
    signed = -magnitude if negative else magnitude
    return (0, max(signed, CLOCK_RATE_STOPPED_PPM)) if rate else (signed, 0)


def park_edges(rng):
    low = 1 << rng.randrange(PARK_EDGE_EXPONENTS)
    return low + rng.randrange(low)


def park_hold_us(rng, ticks):
    low = 1 << rng.randrange(ticks.bit_length())
    high = min(ticks, (low << 1) - 1)
    return (low + rng.randrange(high - low + 1)) * TICK_US


def action(rng, operation):
    return {
        "operation": operation,
        "coverage_quantum": 1 << rng.randrange(QUANTUM_SCALES),
        "choice": rng.getrandbits(64) or 1,
    }


def waits(rng, ticks):
    out = []
    while ticks > 0:
        out.append(action(rng, {"Wait": min(ticks, 1 << MAX_SCALE)}))
        ticks -= 1 << MAX_SCALE
    return out


def operation(rng, vocab, kind, ticks):
    node = rng.randrange(vocab["nodes"])
    if kind == "wait":
        return {"Wait": ticks}
    if kind in NODE_KINDS:
        return {NODE_KINDS[kind]: [node, ticks]}
    if kind in DISK_KINDS:
        return {DISK_KINDS[kind]: [rng.choice(vocab["disks"]), ticks]}
    if kind in ALL_KINDS:
        return {ALL_KINDS[kind]: ticks}
    if kind == "hook":
        return {"Hook": [rng.choice(vocab["hooks"]), ticks]}
    if kind == "disk_corrupt":
        return {"DiskCorrupt": [rng.choice(vocab["checksummed"]), ticks]}
    if kind in ("clock", "clock_all"):
        offset_ms, rate_ppm = clock_shift(rng)
        shift = {"offset_ms": offset_ms, "rate_ppm": rate_ppm, "ticks": ticks}
        return {"Clock": {"node": node, **shift}} if kind == "clock" else {"ClockAll": shift}
    if kind == "event_kill":
        return {"EventKill": {"node": node, "rarity": rng.randrange(64), "ticks": ticks}}
    if kind == "event_park":
        return {
            "EventPark": {
                "node": node,
                "edges": park_edges(rng),
                "hold_us": park_hold_us(rng, ticks),
                "ticks": ticks,
            }
        }
    raise ValueError(kind)


class Cluster:
    def __init__(self, nodes, heal_hook):
        self.nodes = nodes
        self.heal_hook = heal_hook
        self.down = set()
        self.cut = False

    def apply(self, kind, op):
        body = next(iter(op.values()))
        if kind in ("kill", "power_loss"):
            self.down.add(body[0])
        elif kind in ALL_KINDS:
            self.down.update(range(self.nodes))
        elif kind == "restart":
            self.down.discard(body[0])
        elif kind == "hook":
            self.cut = body[0] != self.heal_hook

    def heal(self, rng):
        out = [action(rng, {"Restart": [node, HEAL_RESTART_TICKS]}) for node in sorted(self.down)]
        if self.cut and self.heal_hook is not None:
            out.append(action(rng, {"Hook": [self.heal_hook, 1]}))
        self.down.clear()
        self.cut = False
        return out


def dense(rng, vocab, cluster, length_ticks):
    kinds = ["wait"] + offered_kinds(vocab)
    out, elapsed = [], 0
    while elapsed < length_ticks:
        kind = rng.choice(kinds)
        ticks = log_ticks(rng)
        op = operation(rng, vocab, kind, ticks)
        cluster.apply(kind, op)
        out.append(action(rng, op))
        elapsed += ticks
    return out


def sparse(rng, vocab, cluster, length_ticks, interval_ticks):
    kinds = offered_kinds(vocab)
    out, elapsed = [], 0
    while elapsed < length_ticks:
        before = 1 + rng.randrange(2 * interval_ticks)
        kind = rng.choice(kinds)
        ticks = log_ticks(rng)
        op = operation(rng, vocab, kind, ticks)
        after = 1 + rng.randrange(2 * interval_ticks)
        cluster.apply(kind, op)
        out.extend(waits(rng, before))
        out.append(action(rng, op))
        out.extend(waits(rng, after))
        out.extend(cluster.heal(rng))
        elapsed += before + ticks + after
    return out


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--vocabulary", required=True)
    parser.add_argument("--heal-hook", type=int, help="hook id that removes network faults")
    parser.add_argument("--mode", choices=("dense", "sparse"), required=True)
    parser.add_argument("--seed", type=int, required=True)
    parser.add_argument("--length", type=float, required=True, help="seconds of faults")
    parser.add_argument("--interval", type=float, default=10.0, help="sparse mean gap, seconds")
    parser.add_argument("--quiet", type=float, default=10.0, help="seconds after the final heal")
    args = parser.parse_args()
    vocab = parse_vocabulary(args.vocabulary)
    rng = random.Random(args.seed)
    cluster = Cluster(vocab["nodes"], args.heal_hook)
    length_ticks = int(args.length * 1000 / TICK_MS)
    if args.mode == "dense":
        out = dense(rng, vocab, cluster, length_ticks)
    else:
        out = sparse(rng, vocab, cluster, length_ticks, int(args.interval * 1000 / TICK_MS))
    out.extend(cluster.heal(rng))
    out.extend(waits(rng, int(args.quiet * 1000 / TICK_MS)))
    json.dump(out, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
