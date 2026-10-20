#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Compare one retained NES executable at a fixed logical admission window."""

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import signal
import statistics
import subprocess
import time


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def read_json(path):
    return json.loads(path.read_text())


def logical_digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def validate_result(directory):
    result = read_json(directory / "result.json")
    report = read_json(directory / "campaign.json")
    if result.get("status") != "complete" or result.get("campaign_replay") is not True:
        raise ValueError("run did not complete full campaign replay")
    if result.get("stop_reason") == "wall_limit":
        raise ValueError("wall-limited work is not comparable")
    if result.get("witness_replays") != 2 or result.get("executions", 0) <= 0:
        raise ValueError("missing witness replays or executed work")
    stream = directory / "stream.jsonl"
    if stream.stat().st_size != result["stream_bytes_generated"]:
        raise ValueError("retained stream length mismatch")
    if digest(stream) != result["stream_sha256"] or result["stream_sha256"] != report["stream_sha256"]:
        raise ValueError("retained stream digest mismatch")
    telemetry = read_json(directory / "telemetry.json")
    if sum(worker["jobs"] for worker in telemetry["workers"]) != result["executions"]:
        raise ValueError("worker telemetry does not cover completed work")
    return result, telemetry, {
        "stream": digest(stream),
        "report": logical_digest(report),
        "checkpoint": digest(directory / "checkpoint.bin"),
    }


def run_trial(command, directory, timeout):
    started = time.monotonic()
    with (directory / "execution.log").open("wb") as log:
        process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = process.wait(timeout=timeout)
        except BaseException:
            # Only the process group created by this invocation is signalled.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            raise
    if code:
        raise ValueError(f"evaluator exited {code}; see {directory / 'execution.log'}")
    return time.monotonic() - started


def coordination_costs(telemetry, executions):
    phase = telemetry.get("dispatch_phase") or {}
    host = phase.get("host") or {}
    stream = telemetry.get("stream") or {}
    return {
        "dispatch_phase_ns": phase.get("wall_ns"),
        "coordinator_cpu_ns_per_job": (
            host["coordinator_cpu_ns"] / executions if "coordinator_cpu_ns" in host else None
        ),
        "encoder_cpu_ns_per_job": (
            stream["worker_cpu_ns"] / executions if stream.get("worker_cpu_ns") is not None else None
        ),
        "producer_wait_ns_per_job": (
            stream["producer_wait_ns"] / executions if "producer_wait_ns" in stream else None
        ),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--evaluator", required=True, type=Path)
    parser.add_argument("--request", required=True, type=Path, help="nes-eval request; asset paths relative to current directory")
    parser.add_argument("--out", required=True, type=Path, help="new evidence directory outside the repository")
    parser.add_argument("--workers", nargs="+", type=int, default=[1, 2, 4, 8])
    parser.add_argument("--window", type=int, default=512, help="absolute reservation window, held fixed for all trials")
    parser.add_argument("--repeats", type=int, default=3)
    parser.add_argument("--timeout", type=int, default=300, help="whole evaluator process timeout per trial")
    parser.add_argument("--allow-oversubscription", action="store_true")
    args = parser.parse_args()
    workers = sorted(set(args.workers))
    affinity = sorted(os.sched_getaffinity(0)) if hasattr(os, "sched_getaffinity") else None
    available = len(affinity) if affinity is not None else os.cpu_count()
    if not workers or workers[0] != 1 or args.repeats < 1 or args.timeout < 1:
        parser.error("include one worker, with positive repeats and timeout")
    if args.window < 1 or any(count < 1 or args.window % count for count in workers):
        parser.error("every positive worker count must divide the absolute window")
    if available and max(workers) > available and not args.allow_oversubscription:
        parser.error("workers exceed available CPUs; use --allow-oversubscription only for functional checks")
    request = read_json(args.request)
    if not 1 <= request.get("executions", 0) <= 10000:
        parser.error("executions must be 1..10000 for full stream replay")
    if request.get("resume") or request.get("live") or request.get("live_site"):
        parser.error("use a fresh non-live campaign request")
    for asset in ("rom", "core", "root_input"):
        if request.get(asset):
            request[asset] = str(Path(request[asset]).resolve(strict=True))
    evaluator = args.evaluator.resolve(strict=True)
    evaluator_sha256 = digest(evaluator)
    args.out.mkdir(parents=True, exist_ok=False)
    summary = {
        "format": "nes-scaling-v1", "status": "running",
        "executable": str(evaluator), "executable_sha256": evaluator_sha256,
        "qualifier_sha256": digest(Path(__file__)),
        "host": platform.uname()._asdict(), "affinity": affinity,
        "cpu_count": os.cpu_count(), "absolute_window": args.window,
        "oversubscribed": bool(available and max(workers) > available),
        "request_sha256": digest(args.request), "trials": [],
        "load_before": os.getloadavg() if hasattr(os, "getloadavg") else None,
    }
    for name in ("cpuinfo", "meminfo", "self/cgroup"):
        path = Path("/proc") / name
        if path.is_file():
            (args.out / (name.replace("/", "-") + ".txt")).write_text(path.read_text())

    def save():
        (args.out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")

    save()
    reference = None
    try:
        for repeat in range(args.repeats):
            order = workers if repeat % 2 == 0 else workers[::-1]
            for count in order:
                if digest(evaluator) != evaluator_sha256:
                    raise ValueError("evaluator changed during qualification")
                directory = args.out / f"r{repeat + 1}-w{count}"
                directory.mkdir()
                configured = dict(request, workers=count, window=args.window // count, verification="campaign")
                request_path = directory / "request.json"
                request_path.write_text(json.dumps(configured, indent=2) + "\n")
                output = directory / "run"
                campaign_seconds = run_trial([str(evaluator), str(request_path), str(output)], directory, args.timeout)
                result, telemetry, identity = validate_result(output)
                if len(telemetry["workers"]) != count:
                    raise ValueError("telemetry worker count differs from requested parallelism")
                if reference is None:
                    reference = identity
                if identity != reference:
                    raise ValueError(f"logical work differs at {directory}; complete streams/reports/checkpoints retained")
                search_ns = telemetry["search_ns"]
                busy_ns = sum(worker["busy_ns"] for worker in telemetry["workers"])
                trial = {
                    "repeat": repeat + 1, "workers": count, "identity": identity,
                    "campaign_seconds": campaign_seconds,
                    "search_seconds": result["search_seconds"],
                    "executions": result["executions"], "frames": result["frames_emulated"],
                    "jobs_per_second": result["executions_per_second"],
                    "busy_core_equivalents": busy_ns / search_ns if search_ns else None,
                    "worker_busy_fraction": busy_ns / (search_ns * count) if search_ns else None,
                    "coordination": coordination_costs(telemetry, result["executions"]),
                    "load_after": os.getloadavg() if hasattr(os, "getloadavg") else None,
                    "identity_record": read_json(output / "identity.json"),
                }
                summary["trials"].append(trial)
                save()
                print(f"{directory.name}: {trial['jobs_per_second']:.1f} jobs/s", flush=True)
        summary["rates"] = {}
        baseline = statistics.median(t["jobs_per_second"] for t in summary["trials"] if t["workers"] == 1)
        for count in workers:
            rates = [t["jobs_per_second"] for t in summary["trials"] if t["workers"] == count]
            median = statistics.median(rates)
            if not math.isfinite(median) or median <= 0:
                raise ValueError("invalid measured throughput")
            summary["rates"][str(count)] = {"median": median, "min": min(rates), "max": max(rates), "speedup": median / baseline}
        summary["status"] = "verified"
    except BaseException as error:
        summary["status"] = "failed"
        summary["error"] = str(error)
        raise
    finally:
        save()


if __name__ == "__main__":
    main()
