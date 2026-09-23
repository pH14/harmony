#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

import argparse
import base64
import hashlib
import json
import os
import resource
import statistics
import subprocess
import time
import urllib.parse
import urllib.request
from pathlib import Path


def check_clickhouse():
    fields = ("HARMONY_CLICKHOUSE_URL", "HARMONY_CLICKHOUSE_USER", "HARMONY_CLICKHOUSE_PASSWORD")
    if any(not os.environ.get(field) for field in fields):
        raise SystemExit("set authenticated ClickHouse URL, user, and password for the healthy mode")
    credentials = os.environ["HARMONY_CLICKHOUSE_USER"] + ":" + os.environ["HARMONY_CLICKHOUSE_PASSWORD"]
    token = base64.b64encode(credentials.encode()).decode()
    address = os.environ["HARMONY_CLICKHOUSE_URL"] + "?" + urllib.parse.urlencode({"query": "SELECT version()"})
    request = urllib.request.Request(address, data=b"", method="POST",
                                     headers={"Authorization": "Basic " + token})
    try:
        with urllib.request.urlopen(request, timeout=3) as response:
            response.read()
    except Exception as error:
        raise SystemExit("authenticated ClickHouse preflight failed") from error


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def rss_kib(pid):
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except FileNotFoundError:
        pass
    return 0


def run(binary, rom, core, output, mode, seed, workers, executions, actions):
    output.mkdir(parents=True, exist_ok=True)
    run_dir = output / f"{seed}-{mode}"
    spool_root = output / f"spool-{seed}-{mode}"
    command = [
        str(binary), "run", "--rom", str(rom), "--core", str(core),
        "--output", str(run_dir), "--spool", str(spool_root),
        "--seed", str(seed), "--workers", str(workers),
        "--executions", str(executions), "--actions", str(actions),
    ]
    if mode == "disabled":
        command.append("--no-telemetry")
    environment = os.environ.copy()
    if mode == "disconnected":
        environment["HARMONY_CLICKHOUSE_URL"] = "http://127.0.0.1:18213"
    cpu_before = resource.getrusage(resource.RUSAGE_CHILDREN)
    started = time.monotonic()
    child = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=environment)
    peak_rss = 0
    while child.poll() is None:
        peak_rss = max(peak_rss, rss_kib(child.pid))
        time.sleep(0.02)
    stdout, stderr = child.communicate()
    elapsed = time.monotonic() - started
    cpu_after = resource.getrusage(resource.RUSAGE_CHILDREN)
    if child.returncode:
        raise RuntimeError(f"{mode} exited {child.returncode}: {stderr[-1000:]}")
    payload = json.loads(stdout)
    spool_dir = Path(payload["spool"])
    spool_bytes = sum(path.stat().st_size for path in spool_dir.glob("*.jsonl")) if spool_dir.exists() else 0
    return {
        "mode": mode, "seed": seed, "workers": workers,
        "search_wall_ms": payload["search_wall_ms"],
        "drain_wall_ms": payload["drain_wall_ms"],
        "process_wall_ms": round(elapsed * 1000, 1),
        "cpu_ms": round((cpu_after.ru_utime + cpu_after.ru_stime - cpu_before.ru_utime - cpu_before.ru_stime) * 1000, 1),
        "peak_rss_kib": peak_rss,
        "spool_bytes": spool_bytes,
        "producer_queue_loss": payload["producer_queue_loss"],
        "spool_loss": payload["spool_loss"],
        "executions": payload["executions"],
        "execution_work": payload["execution_work"],
        "stream_sha256": digest(run_dir / "stream.jsonl"),
        "report_sha256": digest(run_dir / "report.json"),
        "run_id": payload["run_id"],
    }


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--rom", type=Path, required=True)
    parser.add_argument("--core", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--binary", type=Path, default=Path("workloads/nes-observatory/target/release/nes-observatory"))
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--workers", type=int, default=2)
    parser.add_argument("--executions", type=int, default=1000)
    parser.add_argument("--actions", type=int, default=256)
    args = parser.parse_args()
    if not 1 <= args.repeats <= 10:
        raise SystemExit("repeats must be 1 through 10")
    if not args.binary.is_file() or not args.rom.is_file() or not args.core.is_file():
        raise SystemExit("binary, ROM, or core is missing")
    check_clickhouse()
    args.output.mkdir(parents=True, exist_ok=True)
    records = []
    for pair in range(args.repeats):
        modes = ("disabled", "healthy", "disconnected")
        modes = modes[pair % 3:] + modes[:pair % 3]
        for mode in modes:
            record = run(args.binary, args.rom, args.core, args.output, mode, 20260921 + pair, args.workers, args.executions, args.actions)
            records.append(record)
            print(json.dumps(record), flush=True)
    comparisons = []
    for pair in range(args.repeats):
        group = [record for record in records if record["seed"] == 20260921 + pair]
        hashes = {(record["stream_sha256"], record["report_sha256"]) for record in group}
        baseline = next(record for record in group if record["mode"] == "disabled")
        healthy = next(record for record in group if record["mode"] == "healthy")
        disconnected = next(record for record in group if record["mode"] == "disconnected")
        comparisons.append({
            "seed": baseline["seed"],
            "deterministic_outputs_equal": len(hashes) == 1,
            "healthy_search_overhead_percent": round((healthy["search_wall_ms"] / baseline["search_wall_ms"] - 1) * 100, 2),
            "disconnected_search_overhead_percent": round((disconnected["search_wall_ms"] / baseline["search_wall_ms"] - 1) * 100, 2),
        })
    summary = {
        "host": os.uname().nodename,
        "workers": args.workers,
        "executions": args.executions,
        "actions": args.actions,
        "repeats": args.repeats,
        "comparisons": comparisons,
        "healthy_median_overhead_percent": statistics.median(item["healthy_search_overhead_percent"] for item in comparisons),
        "disconnected_median_overhead_percent": statistics.median(item["disconnected_search_overhead_percent"] for item in comparisons),
    }
    (args.output / "results.json").write_text(json.dumps({"runs": records, "summary": summary}, indent=2) + "\n")
    print(json.dumps(summary), flush=True)
    if not all(item["deterministic_outputs_equal"] for item in comparisons):
        raise SystemExit("campaign streams or reports differed")


if __name__ == "__main__":
    main()
