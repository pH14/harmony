#!/usr/bin/env -S uv run --script
# SPDX-License-Identifier: AGPL-3.0-or-later
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""Measure searcher throughput, core scaling and memory limits on scaled tiny-world runs."""
from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import secrets
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent
MIB = 1 << 20


def graph(nodes: int, places: int, levels: int) -> dict:
    return {"family": "graph", "parameters": {
        "nodes": nodes, "places": places, "levels": levels, "layout": secrets.randbits(64)}}


def scale(workers: int, memory_mib: int, cost_ns: int, snapshot_bytes: int) -> dict:
    return {"workers": workers, "reservations_per_worker": 2, "memory_budget_mib": memory_mib,
            "archive_entries": 4_194_304, "action_cost_ns": cost_ns, "snapshot_bytes": snapshot_bytes}


def clock(text: str) -> float:
    seconds = 0.0
    for part in text.split(":"):
        seconds = seconds * 60 + float(part)
    return seconds


def coordinator_cpu(pid: int) -> float | None:
    if sys.platform == "linux":
        try:
            return int(Path(f"/proc/{pid}/task/{pid}/schedstat").read_text().split()[0]) / 1e9
        except OSError:
            return None
    rows = subprocess.run(["ps", "-M", "-p", str(pid)], capture_output=True, text=True).stdout.splitlines()
    if len(rows) < 2:
        return None
    fields = rows[1].split()
    return clock(fields[6]) + clock(fields[7])


def launch(binary: Path, config: dict, settings: dict, work: int, sample_seconds: float) -> dict:
    request = {"config": config, "seed": secrets.randbits(64), "work_budget": work, "broken": False,
               "verify": False, "keep": "portfolio", "scale": settings}
    proc = subprocess.Popen([str(binary)], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, text=True,
                            env=dict(os.environ, HARMONY_COORDINATOR_PROFILE="1"))
    proc.stdin.write(json.dumps(request))
    proc.stdin.close()
    latest: dict = {}
    peaks = {"resident_memory_bytes": 0, "lines_over_budget": 0}
    out: list[str] = []
    budget = settings["memory_budget_mib"] * MIB

    def read_progress() -> None:
        for line in proc.stderr:
            if not line.startswith("{"):
                continue
            record = json.loads(line)
            latest.update({k: record.get(k, 0) for k in (
                "executions", "execution_work", "active_entries", "live_entries", "resident_snapshots",
                "resident_memory_bytes", "snapshot_evictions", "entry_drops", "history_compactions",
                "historical_entries_dropped", "objectives_reached")})
            peaks["resident_memory_bytes"] = max(peaks["resident_memory_bytes"], record["resident_memory_bytes"])
            peaks["lines_over_budget"] += record["resident_memory_bytes"] > budget

    readers = [threading.Thread(target=read_progress),
               threading.Thread(target=lambda: out.append(proc.stdout.read()))]
    for reader in readers:
        reader.start()
    samples = []
    started = time.monotonic()
    while True:
        pid, status, usage = os.wait4(proc.pid, os.WNOHANG)
        if pid:
            break
        cpu = coordinator_cpu(proc.pid)
        if cpu is not None and latest:
            samples.append({"seconds": time.monotonic() - started, "coordinator_cpu": cpu, **latest})
        time.sleep(sample_seconds)
    for reader in readers:
        reader.join()
    if status:
        raise SystemExit(f"tiny-worlds failed with status {status}")
    report = json.loads(out[0])
    peak_rss = usage.ru_maxrss if sys.platform == "darwin" else usage.ru_maxrss * 1024
    return {"report": report, "samples": samples, "final": dict(latest), "peak_rss_bytes": peak_rss, **peaks}


def windows(samples: list[dict]) -> list[dict]:
    rows = []
    for a, b in zip(samples, samples[1:]):
        executions = b["executions"] - a["executions"]
        if executions <= 0:
            continue
        rows.append({"active_entries": b["active_entries"],
                     "executions_per_second": executions / (b["seconds"] - a["seconds"]),
                     "coordinator_ms_per_1000": (b["coordinator_cpu"] - a["coordinator_cpu"]) * 1e6 / executions})
    return rows


def slowdown(args: argparse.Namespace, binaries: list[Path]) -> None:
    config = graph(4_194_304, 1024, 4)
    settings = scale(args.workers, 16_384, args.cost_ns, 0)
    for binary in binaries:
        for repeat in range(args.repeats):
            run = launch(binary, config, settings, args.work, args.sample_seconds)
            print(f"{binary} repeat {repeat + 1}: {run['final'].get('executions', 0):,} executions, "
                  f"{run['report']['elapsed_seconds']:.0f} s, peak RSS {run['peak_rss_bytes'] / MIB:,.0f} MiB")
            print(f"  {'active entries':>14}  {'executions/s':>12}  {'coordinator ms per 1,000':>24}")
            rows = windows(run["samples"])
            step = max(1, len(rows) // args.rows)
            for i in range(0, len(rows), step):
                chunk = rows[i:i + step]
                print(f"  {chunk[-1]['active_entries']:>14,}  "
                      f"{statistics.median(r['executions_per_second'] for r in chunk):>12,.0f}  "
                      f"{statistics.median(r['coordinator_ms_per_1000'] for r in chunk):>24,.0f}")


def cores(args: argparse.Namespace, binary: Path) -> None:
    config = graph(4_194_304, 1024, 4)
    print(f"cost {args.cost_ns:,} ns per transition, work {args.work:,} transitions per worker")
    print(f"{'workers':>7}  {'executions/s':>12}  {'speedup':>7}  {'coordinator busy':>16}  {'worker ms per try':>17}")
    base = None
    for workers in args.workers_list:
        rates, busy, per_try = [], [], []
        for _ in range(args.repeats):
            run = launch(binary, config, scale(workers, 16_384, args.cost_ns, 0), args.work * workers,
                         args.sample_seconds)
            final, elapsed = run["final"], run["report"]["elapsed_seconds"]
            rates.append(final["executions"] / elapsed)
            if run["samples"]:
                busy.append(run["samples"][-1]["coordinator_cpu"] / run["samples"][-1]["seconds"])
            per_try.append(args.cost_ns * final["execution_work"] / final["executions"] / 1e6)
        rate = statistics.median(rates)
        base = base or rate
        print(f"{workers:>7}  {rate:>12,.0f}  {rate / base:>7.2f}  "
              f"{statistics.median(busy) if busy else float('nan'):>16.0%}  {statistics.median(per_try):>17.2f}")


def memory(args: argparse.Namespace, binary: Path) -> None:
    config = graph(65_536, 64, 4)
    budget = scale(args.workers, args.budget_mib, 0, args.snapshot_bytes)
    unbounded = scale(args.workers, 16_384, 0, args.snapshot_bytes)
    with concurrent.futures.ThreadPoolExecutor(args.jobs) as pool:
        control = pool.submit(launch, binary, config, unbounded, args.work, args.sample_seconds)
        runs = list(pool.map(lambda _: launch(binary, config, budget, args.work, args.sample_seconds),
                             range(args.seeds)))
        control = control.result()
    print(f"budget {args.budget_mib:,} MiB, snapshot {args.snapshot_bytes:,} bytes; "
          f"unbounded control peaks at {control['resident_memory_bytes'] / MIB:,.0f} MiB logical, "
          f"{control['peak_rss_bytes'] / MIB:,.0f} MiB RSS")
    print(f"{'goal':>4}  {'first goal work':>15}  {'peak logical MiB':>16}  {'lines over':>10}  {'peak RSS MiB':>12}  "
          f"{'evictions':>9}  {'compactions':>11}  {'entries dropped':>15}")
    for run in runs:
        final, report = run["final"], run["report"]
        print(f"{'yes' if report['success'] else 'no':>4}  {report['first_objective_work'] or 0:>15,}  "
              f"{run['resident_memory_bytes'] / MIB:>16,.0f}  {run['lines_over_budget']:>10}  "
              f"{run['peak_rss_bytes'] / MIB:>12,.0f}  {final['snapshot_evictions']:>9,}  "
              f"{final['history_compactions']:>11,}  {final['historical_entries_dropped']:>15,}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["slowdown", "cores", "memory"])
    parser.add_argument("--binary", type=Path, action="append",
                        help="prebuilt tiny-worlds executable; slowdown accepts several to compare")
    parser.add_argument("--workers", type=int, default=8, help="worker threads (slowdown and memory)")
    parser.add_argument("--workers-list", type=lambda s: [int(w) for w in s.split(",")], default=[1, 2, 4, 8],
                        help="worker counts for cores")
    parser.add_argument("--cost-ns", type=int, default=0, help="CPU time per transition")
    parser.add_argument("--work", type=int, default=6_000_000,
                        help="transition budget per run (per worker for cores)")
    parser.add_argument("--repeats", type=int, default=1, help="timing repeats per setting")
    parser.add_argument("--rows", type=int, default=12, help="table rows per slowdown run")
    parser.add_argument("--budget-mib", type=int, default=256, help="memory budget for memory mode")
    parser.add_argument("--snapshot-bytes", type=int, default=21_238, help="snapshot payload for memory mode")
    parser.add_argument("--seeds", type=int, default=6, help="runtime seeds for memory mode")
    parser.add_argument("--jobs", type=int, default=3, help="parallel processes for memory mode")
    parser.add_argument("--sample-seconds", type=float, default=2.0, help="coordinator CPU sampling interval")
    args = parser.parse_args()
    binaries = args.binary
    if not binaries:
        subprocess.run(["cargo", "build", "--release", "--locked", "--manifest-path",
                        str(ROOT / "Cargo.toml")], check=True)
        binaries = [ROOT / "target" / "release" / "tiny-worlds"]
    if args.mode == "slowdown":
        slowdown(args, binaries)
    elif args.mode == "cores":
        cores(args, binaries[0])
    else:
        memory(args, binaries[0])
    return 0


if __name__ == "__main__":
    sys.exit(main())
