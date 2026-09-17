#!/usr/bin/env python3
"""Validate historical case manifests and emit a GitHub Actions matrix.

The historical panel is intentionally driven by the case directories.  A case
whose workload is not present on the current branch can remain visible as a
``deferred`` entry with a concrete reason; it is never represented by a
successful empty job.

A case pins the affected version of its software.  Upstream fix references stay
in the manifest as provenance and are never executed: a search runs the affected
version, confirms its own finding, and replays the declared clean samples on
that same version.
"""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
VALID_CI_STATES = {"runnable", "deferred"}
GITHUB_JOB_LIMIT_MINUTES = 360
SEARCH_HEADROOM_MINUTES = 20


def cases() -> list[tuple[Path, dict]]:
    result: list[tuple[Path, dict]] = []
    for path in sorted((ROOT / "workloads/bugs/historical").glob("*/case.json")):
        try:
            case = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError) as error:
            raise SystemExit(f"{path}: cannot read JSON: {error}") from error
        result.append((path, case))
    if not result:
        raise SystemExit("no workloads/bugs/historical/*/case.json files found")
    return result


def require(case: dict, path: Path, *keys: str) -> None:
    value: object = case
    for key in keys:
        if not isinstance(value, dict) or key not in value:
            raise SystemExit(f"{path}: missing required field {'.'.join(keys)}")
        value = value[key]


def validate(path: Path, case: dict) -> dict:
    for key in ("id", "software", "workload", "oracle", "run", "search"):
        require(case, path, key)
    if not isinstance(case["id"], str) or not case["id"]:
        raise SystemExit(f"{path}: id must be a non-empty string")
    if Path(case["id"]).name != case["id"] or case["id"] in {".", ".."}:
        raise SystemExit(f"{path}: id must be one path component")
    if "arms" in case:
        raise SystemExit(f"{path}: a case pins one affected version; arms are not supported")
    require(case, path, "workload", "version")
    if not isinstance(case["workload"]["version"], str) or not case["workload"]["version"]:
        raise SystemExit(f"{path}: workload.version must be a non-empty string")
    for key in ("assertion", "evidence"):
        require(case, path, "oracle", key)
    for key in ("ram_mib",):
        require(case, path, "run", key)
    for key in ("workers", "actions", "wall_minutes"):
        require(case, path, "search", key)
    # A seed is an input to a sampling procedure, so the run supplies one and
    # records it. Committing one invites an expectation the next code change
    # breaks.
    if "seed" in case["search"]:
        raise SystemExit(f"{path}: search.seed is supplied by the run, not the case")
    for group, keys in (
        ("run", ("ram_mib",)),
        ("search", ("workers", "actions", "wall_minutes")),
    ):
        for key in keys:
            value = case[group][key]
            if not isinstance(value, int) or value <= 0:
                raise SystemExit(f"{path}: {group}.{key} must be a positive integer")
    executions = case["search"].get("executions", 0)
    if not isinstance(executions, int) or executions < 0:
        raise SystemExit(f"{path}: search.executions must be a non-negative integer")

    ci = case.get("ci", {})
    if not isinstance(ci, dict):
        raise SystemExit(f"{path}: ci must be an object")
    if "search_arms" in ci:
        raise SystemExit(f"{path}: ci.search_arms is not supported; a case searches its one version")
    state = ci.get("status", "runnable")
    if state not in VALID_CI_STATES:
        raise SystemExit(f"{path}: ci.status must be runnable or deferred")
    if state == "deferred" and not isinstance(ci.get("reason"), str):
        raise SystemExit(f"{path}: deferred cases need ci.reason")
    if state == "runnable":
        require(case, path, "image", "context")
        require(case, path, "image", "dockerfile")
        require(case, path, "ci", "display_name")
        if executions <= 0:
            raise SystemExit(f"{path}: runnable cases need a positive search.executions")
    display_name = ci.get("display_name", case["software"])
    if not isinstance(display_name, str) or not display_name:
        raise SystemExit(f"{path}: ci.display_name must be a non-empty string")
    prefix = ci.get("image_prefix", case["id"])
    if not isinstance(prefix, str) or not prefix:
        raise SystemExit(f"{path}: ci.image_prefix must be a non-empty string")
    samples = case.get("samples", [])
    if not isinstance(samples, list):
        raise SystemExit(f"{path}: samples must be an array")
    sample_paths: list[str] = []
    for sample in samples:
        if not isinstance(sample, dict) or not isinstance(sample.get("path"), str):
            raise SystemExit(f"{path}: every sample needs a path")
        if sample.get("expect", "clean") != "clean":
            raise SystemExit(f"{path}: only clean samples are supported")
        sample_path = ROOT / sample["path"]
        if state == "runnable" and not sample_path.is_file():
            raise SystemExit(f"{path}: sample does not exist: {sample['path']}")
        sample_paths.append(sample["path"])
    planned_replay_sessions = 2 * len(sample_paths)
    replay_max_sessions = ci.get("replay_max_sessions", max(planned_replay_sessions, 1))
    replay_timeout_seconds = ci.get("replay_timeout_seconds", 1800)
    for key, value in (
        ("replay_max_sessions", replay_max_sessions),
        ("replay_timeout_seconds", replay_timeout_seconds),
    ):
        if not isinstance(value, int) or value <= 0:
            raise SystemExit(f"{path}: ci.{key} must be a positive integer")
    if state == "runnable" and replay_max_sessions < planned_replay_sessions:
        raise SystemExit(
            f"{path}: ci.replay_max_sessions is below the planned aggregate "
            f"replay count ({planned_replay_sessions})"
        )

    replay_minutes = math.ceil(planned_replay_sessions * replay_timeout_seconds / 60)
    job_timeout_minutes = (
        case["search"]["wall_minutes"] + SEARCH_HEADROOM_MINUTES + replay_minutes
    )
    if job_timeout_minutes > GITHUB_JOB_LIMIT_MINUTES:
        raise SystemExit(
            f"{path}: search wall budget, {SEARCH_HEADROOM_MINUTES}-minute headroom and "
            f"declared sample replays exceed GitHub's {GITHUB_JOB_LIMIT_MINUTES}-minute job limit"
        )

    return {
        "id": case["id"],
        "dir": str(path.parent.relative_to(ROOT)),
        "software": case["software"],
        "display_name": display_name,
        "ci_status": state,
        "ci_reason": ci.get("reason", ""),
        "image_prefix": prefix,
        "workload_version": case["workload"]["version"],
        "oracle_assertion": case["oracle"]["assertion"],
        "oracle_evidence": case["oracle"]["evidence"],
        "ram_mib": case["run"]["ram_mib"],
        "workers": case["search"]["workers"],
        "actions": case["search"]["actions"],
        "wall_minutes": case["search"]["wall_minutes"],
        "job_timeout_minutes": job_timeout_minutes,
        "executions": executions,
        "knobs": " ".join(
            f"{key}={value}" for key, value in case.get("run", {}).get("knobs", {}).items()
        ),
        "sample_paths": sample_paths,
        "replay_max_sessions": replay_max_sessions,
        "replay_timeout_seconds": replay_timeout_seconds,
        "planned_replay_sessions": planned_replay_sessions,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--matrix", action="store_true", help="emit a GitHub matrix JSON object")
    parser.add_argument(
        "--runnable-matrix",
        action="store_true",
        help="emit a matrix containing only runnable cases",
    )
    parser.add_argument("--check", action="store_true", help="validate manifests without output")
    args = parser.parse_args()
    entries = [validate(path, case) for path, case in cases()]
    if args.matrix and args.runnable_matrix:
        parser.error("choose only one matrix mode")
    if args.matrix or args.runnable_matrix:
        selected = entries if args.matrix else [entry for entry in entries if entry["ci_status"] == "runnable"]
        json.dump({"include": selected}, sys.stdout, separators=(",", ":"))
        sys.stdout.write("\n")
    elif not args.check:
        for entry in entries:
            state = entry["ci_status"]
            suffix = f": {entry['ci_reason']}" if state == "deferred" else ""
            print(f"{entry['id']}\t{state}{suffix}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
