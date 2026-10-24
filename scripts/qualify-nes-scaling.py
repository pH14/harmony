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


def counter(value):
    if type(value) is not int or not 0 <= value <= (1 << 64) - 1:
        raise ValueError("invalid unsigned counter")
    return value


def checked_sum(values):
    return counter(sum(counter(value) for value in values))


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
    if result.get("engine") is not None:
        raise ValueError("unexpected evaluator engine")
    work = counter(result["executions"])
    if counter(report["executions_completed"]) != work:
        raise ValueError("result executions differ from completed campaign jobs")
    profile_provenance(telemetry)
    if checked_sum(worker["jobs"] for worker in telemetry["workers"]) != work:
        raise ValueError("worker telemetry does not cover completed work")
    return result, telemetry, {
        "stream": digest(stream),
        "report": logical_digest(report),
        "checkpoint": digest(directory / "checkpoint.bin"),
    }


def run_trial(command, directory, timeout, environment):
    started = time.monotonic()
    with (directory / "execution.log").open("wb") as log:
        process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True, env=environment)
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


PROFILE_ROLES = {
    "off": (False, False),
    "coordinator_only": (True, False),
    "worker_only": (False, True),
    "coordinator_and_worker": (True, True),
}


def requested_profiling(environment):
    roles = ("HARMONY_COORDINATOR_PROFILE" in environment,
             "HARMONY_WORKER_CPU_PROFILE" in environment)
    return next(mode for mode, enabled in PROFILE_ROLES.items() if enabled == roles)


def profile_provenance(telemetry):
    workers = telemetry.get("workers")
    if not isinstance(workers, list) or not workers:
        raise ValueError("worker profiling provenance is missing")
    modes = []
    for worker in workers:
        if not isinstance(worker, dict):
            raise ValueError("worker profiling provenance must be an object")
        mode = worker.get("profile_mode")
        if not isinstance(mode, str) or mode not in PROFILE_ROLES:
            raise ValueError("worker profiling mode is missing or unknown")
        if worker.get("execute_cpu") is not None and not isinstance(worker["execute_cpu"], dict):
            raise ValueError("worker execute CPU must be an object")
        if (worker.get("execute_cpu") is not None) != PROFILE_ROLES[mode][1]:
            raise ValueError("worker profiling mode contradicts execute CPU presence")
        modes.append(mode)
    if len(set(modes)) != 1:
        raise ValueError("worker profiling modes differ")
    coordinator, worker = PROFILE_ROLES[modes[0]]
    detail = telemetry.get("coordinator")
    if detail is None:
        if coordinator:
            raise ValueError("coordinator profiling mode lacks diagnostics")
    elif not isinstance(detail, dict):
        raise ValueError("coordinator diagnostics must be an object")
    elif "kind" in detail:
        raise ValueError("unknown coordinator diagnostics kind")
    else:
        fields = ("admission_ns", "bookkeeping_ns", "history_compaction_ns",
                  "stream_write_ns", "selection_ns")
        complete = detail.get("timing_complete")
        if type(complete) is not bool or any(field not in detail for field in fields):
            raise ValueError("unrecognized linear coordinator diagnostics")
        if not complete:
            if any(detail[field] is not None for field in fields):
                raise ValueError("incomplete linear timing contains verbose durations")
        elif (not coordinator or any(type(detail[field]) is not int or not 0 <= detail[field] < 1 << 128
                                     for field in fields)):
            raise ValueError("linear timing contradicts coordinator profiling mode")
    return {"profiling_mode": modes[0], "coordinator_verbose_enabled": coordinator,
            "worker_execute_cpu_enabled": worker}


def worker_execute_cost(telemetry, jobs):
    if counter(jobs) == 0:
        raise ValueError("worker execute CPU denominator must be positive")
    provenance = profile_provenance(telemetry)
    workers = telemetry["workers"]
    if not provenance["worker_execute_cpu_enabled"]:
        return None
    total_cpu = 0
    total_calls = 0
    usable = True
    for worker in workers:
        value = worker["execute_cpu"]
        if (value.get("clock") != "thread_cpu_time" or value.get("scope") !=
                "actual_worker_execute_including_unwind_excluding_queue_reply_and_post_job_telemetry"):
            raise ValueError("worker execute CPU boundary differs")
        for field in ("supported", "complete", "counter_overflow"):
            if type(value.get(field)) is not bool:
                raise ValueError("invalid worker execute CPU coverage flag")
        calls, samples, errors, panics = (counter(value[field]) for field in
                                           ("calls", "samples", "errors", "panics"))
        if samples > calls or errors + panics > calls:
            raise ValueError("worker execute CPU classifications differ")
        cpu = value.get("total_cpu_ns")
        if cpu is not None and (type(cpu) is not int or not 0 <= cpu < 1 << 128):
            raise ValueError("invalid worker execute CPU duration")
        complete = (value["supported"] and not value["counter_overflow"] and
                    calls == counter(worker["jobs"]) and samples == calls and cpu is not None)
        if value["complete"] and not complete:
            raise ValueError("worker execute CPU completeness contradicts coverage")
        usable &= value["complete"] and complete and errors == panics == 0
        total_cpu += cpu or 0
        total_calls += calls
    if not usable:
        return None
    if total_calls != jobs:
        raise ValueError("worker execute CPU denominator differs from admitted work")
    return total_cpu / jobs


def coordination_costs(telemetry, executions):
    if counter(executions) == 0:
        raise ValueError("coordination cost denominator must be positive")
    phase = telemetry.get("dispatch_phase") or {}
    host = phase.get("host") or {}
    whole = telemetry.get("coordinator_host") or {}
    stream = telemetry.get("stream") or {}

    def per_job(value):
        return counter(value) / executions if value is not None else None

    overflow = stream.get("counter_overflow", False)
    if type(overflow) is not bool:
        raise ValueError("invalid stream counter overflow flag")
    def role_cost(row):
        nonlocal overflow
        if not isinstance(row, dict) or type(row.get("counter_overflow", False)) is not bool:
            raise ValueError("invalid stream thread telemetry")
        overflow |= row.get("counter_overflow", False)
        return per_job(row.get("cpu_ns"))

    serializers = stream.get("serializers")
    serializer_costs = None
    if serializers is not None:
        if not isinstance(serializers, list) or len(serializers) != 2:
            raise ValueError("expected two stream serializers")
        serializer_costs = [role_cost(row) for row in serializers]
    relay_cost = role_cost(stream.get("relay") or {})
    bottleneck = None
    if not overflow and serializer_costs is not None and relay_cost is not None:
        if all(value is not None for value in serializer_costs):
            bottleneck = max(*serializer_costs, relay_cost)
    return {
        **profile_provenance(telemetry),
        "dispatch_phase_ns": phase.get("wall_ns"),
        "worker_execute_cpu_ns_per_job": worker_execute_cost(telemetry, executions),
        "coordinator_cpu_ns_per_job": per_job(host.get("coordinator_cpu_ns")),
        "coordinator_whole_cpu_ns_per_job": per_job(whole.get("coordinator_cpu_ns")),
        "coordinator_whole_run_wait_ns_per_job": per_job(whole.get("coordinator_run_wait_ns")),
        "stream_aux_cpu_ns_per_job": (
            None if overflow else per_job(stream.get("worker_cpu_ns"))
        ),
        "serializer_cpu_ns_per_job": serializer_costs,
        "relay_cpu_ns_per_job": relay_cost,
        "stream_dedicated_core_bottleneck_ns_per_job": bottleneck,
        "producer_wait_ns_per_job": per_job(stream.get("producer_wait_ns")),
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
    if args.window < 1 or any(count < 1 for count in workers):
        parser.error("window and worker counts must be positive")
    if any(args.window % count for count in workers):
        parser.error("every worker count must divide the absolute window")
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
    environment = os.environ.copy()
    requested_mode = requested_profiling(environment)
    args.out.mkdir(parents=True, exist_ok=False)
    summary = {
        "format": "nes-scaling-v2", "status": "running",
        "executable": str(evaluator), "executable_sha256": evaluator_sha256,
        "qualifier_sha256": digest(Path(__file__)),
        "requested_profiling_mode": requested_mode,
        "profile_flags_present": {name: name in environment for name in
                                  ("HARMONY_COORDINATOR_PROFILE", "HARMONY_WORKER_CPU_PROFILE")},
        "host": platform.uname()._asdict(), "affinity": affinity,
        "cpu_count": os.cpu_count(), "absolute_window": args.window,
        "window_unit": "outstanding_jobs",
        "throughput_unit": "jobs_per_second",
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
                campaign_seconds = run_trial([str(evaluator), str(request_path), str(output)], directory, args.timeout, environment)
                result, telemetry, identity = validate_result(output)
                provenance = profile_provenance(telemetry)
                if provenance["profiling_mode"] != requested_mode:
                    raise ValueError("actual profiling mode differs from requested environment")
                completed = result["executions"]
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
                    "throughput_per_second": rate, "throughput_unit": "job",
                    "execution_unit": "jobs",
                    "busy_core_equivalents": busy_ns / search_ns if search_ns else None,
                    "worker_busy_fraction": busy_ns / (search_ns * count) if search_ns else None,
                    "coordination": {"unit": "job", **coordination_costs(telemetry, completed)},
                    "load_after": os.getloadavg() if hasattr(os, "getloadavg") else None,
                    "identity_record": read_json(output / "identity.json"),
                }
                summary["trials"].append(trial)
                save()
                print(f"{directory.name}: {rate:.1f} jobs/s", flush=True)
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
