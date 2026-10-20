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


EXPANSION_FIELDS = {
    "tasks_reserved": "tasks_reserved", "tasks_admitted": "tasks_admitted",
    "edge_attempts": "edge_attempts", "budget_consumed": "budget_consumed",
    "executed_edges": "executed_edges", "preparation_failures": "preparation_failures",
    "empty_tasks": "empty_tasks", "bootstrap_work": "bootstrap_frames",
    "reconstruction_work": "reconstruction_frames", "execution_work": "edge_frames",
}


def counter(value):
    if type(value) is not int or not 0 <= value <= (1 << 64) - 1:
        raise ValueError("invalid unsigned counter")
    return value


def checked_sum(values):
    return counter(sum(counter(value) for value in values))


def expansion_deltas(result, report):
    if result.get("window_semantics") != "fixed_outstanding_tasks":
        raise ValueError("expansion window semantics missing")
    header = report["header"]
    if (header.get("format") != "dissonance-bounded-expansion-v1"
            or header.get("schedule") != "bounded_beam_fifo_tasks_v1"
            or counter(header["window"]) == 0):
        raise ValueError("expansion report header missing or invalid")
    final, initial = report["counters"], report["initial_counters"]
    if result.get("initial_counters") != initial:
        raise ValueError("expansion initial counters differ from report")
    deltas = {}
    for source, target in EXPANSION_FIELDS.items():
        current, previous = counter(final[source]), counter(initial[source])
        counter(result["initial_counters"][source])
        if current < previous:
            raise ValueError("expansion session counter moved backwards")
        deltas[target] = current - previous
        result_field = "executions" if source == "executed_edges" else target
        if counter(result[result_field]) != current:
            raise ValueError("expansion result counters differ from report")
    deltas["frames_emulated"] = checked_sum(deltas[name] for name in (
        "bootstrap_frames", "reconstruction_frames", "edge_frames"))
    recorded_deltas = result.get("session_counter_deltas")
    if not isinstance(recorded_deltas, dict):
        raise ValueError("expansion session deltas missing")
    for value in recorded_deltas.values():
        counter(value)
    if recorded_deltas != deltas:
        raise ValueError("expansion session deltas differ from report")
    if counter(result["frames_emulated"]) != checked_sum(final[name] for name in (
        "bootstrap_work", "reconstruction_work", "execution_work")):
        raise ValueError("expansion cumulative frame work differs from report")
    if (final["executed_edges"] > final["edge_attempts"]
            or final["edge_attempts"] + final["empty_tasks"] != final["budget_consumed"]
            or final["tasks_admitted"] != final["tasks_reserved"]
            or final["tasks_admitted"] > final["budget_consumed"]
            or final["budget_consumed"] > 14 * final["tasks_admitted"]
            or final["preparation_failures"] < final["edge_attempts"] - final["executed_edges"]
            or final["preparation_failures"] - (final["edge_attempts"] - final["executed_edges"]) > final["empty_tasks"]):
        raise ValueError("expansion counters violate task accounting")
    if result.get("rate_basis") != "session_counter_deltas_over_search_seconds":
        raise ValueError("expansion rate basis missing")
    if result.get("rates_include_startup_and_finalization_wall") is not True:
        raise ValueError("expansion rate wall scope missing")
    if deltas["tasks_admitted"] <= 0:
        raise ValueError("no admitted session tasks")
    return deltas


def validate_result(directory, expected_engine=None):
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
    engine = result.get("engine")
    if expected_engine is not None and engine != expected_engine:
        raise ValueError("evaluator engine differs from requested route")
    if engine not in (None, "bounded_expansion"):
        raise ValueError("unknown evaluator engine")
    if engine == "bounded_expansion":
        work = expansion_deltas(result, report)["tasks_admitted"]
    else:
        work = counter(result["executions"])
    if checked_sum(worker["jobs"] for worker in telemetry["workers"]) != work:
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


def coordination_costs(telemetry, executions, unit="job"):
    phase = telemetry.get("dispatch_phase") or {}
    host = phase.get("host") or {}
    stream = telemetry.get("stream") or {}
    return {
        "dispatch_phase_ns": phase.get("wall_ns"),
        f"coordinator_cpu_ns_per_{unit}": (
            host["coordinator_cpu_ns"] / executions if "coordinator_cpu_ns" in host else None
        ),
        f"encoder_cpu_ns_per_{unit}": (
            stream["worker_cpu_ns"] / executions if stream.get("worker_cpu_ns") is not None else None
        ),
        f"producer_wait_ns_per_{unit}": (
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
    request = read_json(args.request)
    expansion = request.get("game") == "famidash"
    if args.window < 1 or any(count < 1 for count in workers):
        parser.error("window and worker counts must be positive")
    if not expansion and any(args.window % count for count in workers):
        parser.error("every linear-route worker count must divide the absolute window")
    if available and max(workers) > available and not args.allow_oversubscription:
        parser.error("workers exceed available CPUs; use --allow-oversubscription only for functional checks")
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
        "format": "nes-scaling-v2", "status": "running",
        "executable": str(evaluator), "executable_sha256": evaluator_sha256,
        "qualifier_sha256": digest(Path(__file__)),
        "host": platform.uname()._asdict(), "affinity": affinity,
        "cpu_count": os.cpu_count(), "absolute_window": args.window,
        "engine": "bounded_expansion" if expansion else "linear",
        "window_unit": "outstanding_tasks" if expansion else "outstanding_jobs",
        "throughput_unit": "tasks_per_second" if expansion else "jobs_per_second",
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
                configured = dict(request, workers=count, window=args.window if expansion else args.window // count, verification="campaign")
                request_path = directory / "request.json"
                request_path.write_text(json.dumps(configured, indent=2) + "\n")
                output = directory / "run"
                campaign_seconds = run_trial([str(evaluator), str(request_path), str(output)], directory, args.timeout)
                result, telemetry, identity = validate_result(
                    output, "bounded_expansion" if expansion else None)
                if not expansion and result.get("engine") is not None:
                    raise ValueError("linear request returned expansion engine")
                deltas = expansion_deltas(result, read_json(output / "campaign.json")) if expansion else None
                if expansion:
                    header = read_json(output / "campaign.json")["header"]
                    if header["window"] != args.window or header["execution_budget"] != request["executions"]:
                        raise ValueError("expansion report window or budget differs from request")
                completed = deltas["tasks_admitted"] if expansion else result["executions"]
                unit = "task" if expansion else "job"
                seconds = result["search_seconds"]
                if not math.isfinite(seconds) or seconds <= 0:
                    raise ValueError("invalid search duration")
                rate = completed / seconds
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
                    "throughput_per_second": rate, "throughput_unit": unit,
                    "completed_session_tasks": completed if expansion else None,
                    "expansion_session": deltas,
                    "expansion_initial": result.get("initial_counters") if expansion else None,
                    "execution_unit": "executed_edges" if expansion else "jobs",
                    "executed_edges_per_second": result["executions_per_second"] if expansion else None,
                    "busy_core_equivalents": busy_ns / search_ns if search_ns else None,
                    "worker_busy_fraction": busy_ns / (search_ns * count) if search_ns else None,
                    "coordination": {"unit": unit, **coordination_costs(telemetry, completed, unit)},
                    "load_after": os.getloadavg() if hasattr(os, "getloadavg") else None,
                    "identity_record": read_json(output / "identity.json"),
                }
                summary["trials"].append(trial)
                save()
                print(f"{directory.name}: {rate:.1f} {unit}s/s", flush=True)
        summary["rates"] = {}
        baseline = statistics.median(t["throughput_per_second"] for t in summary["trials"] if t["workers"] == 1)
        for count in workers:
            rates = [t["throughput_per_second"] for t in summary["trials"] if t["workers"] == count]
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
