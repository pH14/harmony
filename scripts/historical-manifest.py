#!/usr/bin/env python3
"""Validate historical case manifests and emit a GitHub Actions matrix.

The historical panel is intentionally driven by the case directories.  A case
whose workload is not present on the current branch can remain visible as a
``deferred`` entry with a concrete reason; it is never represented by a
successful empty job.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
VALID_CI_STATES = {"runnable", "deferred"}
VALID_ARMS = {"vulnerable", "control"}


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
    for key in ("id", "software", "arms", "oracle", "run", "search"):
        require(case, path, key)
    if not isinstance(case["id"], str) or not case["id"]:
        raise SystemExit(f"{path}: id must be a non-empty string")
    if Path(case["id"]).name != case["id"] or case["id"] in {".", ".."}:
        raise SystemExit(f"{path}: id must be one path component")
    for arm in ("vulnerable", "control"):
        require(case, path, "arms", arm, "version")
    for key in ("assertion", "evidence"):
        require(case, path, "oracle", key)
    for key in ("horizon_ms", "ram_mib"):
        require(case, path, "run", key)
    for key in ("seed", "workers", "actions", "wall_minutes"):
        require(case, path, "search", key)
    for group, keys in (
        ("run", ("horizon_ms", "ram_mib")),
        ("search", ("seed", "workers", "actions", "wall_minutes")),
    ):
        for key in keys:
            value = case[group][key]
            if not isinstance(value, int) or value <= 0:
                raise SystemExit(f"{path}: {group}.{key} must be a positive integer")
    executions = case["search"].get("executions", 0)
    if not isinstance(executions, int) or executions < 0:
        raise SystemExit(f"{path}: search.executions must be a non-negative integer")
    search_timeout_minutes = case["search"]["wall_minutes"] + 20
    if search_timeout_minutes > 360:
        raise SystemExit(
            f"{path}: search wall budget plus 20-minute headroom exceeds GitHub's 360-minute job limit"
        )

    ci = case.get("ci", {})
    if not isinstance(ci, dict):
        raise SystemExit(f"{path}: ci must be an object")
    state = ci.get("status", "runnable")
    if state not in VALID_CI_STATES:
        raise SystemExit(f"{path}: ci.status must be runnable or deferred")
    if state == "deferred" and not isinstance(ci.get("reason"), str):
        raise SystemExit(f"{path}: deferred cases need ci.reason")
    if state == "runnable":
        require(case, path, "image", "context")
        require(case, path, "image", "dockerfile")
        if executions <= 0:
            raise SystemExit(f"{path}: runnable cases need a positive search.executions")
    search_arms = ci.get("search_arms", ["vulnerable"])
    if (
        not isinstance(search_arms, list)
        or not search_arms
        or any(not isinstance(arm, str) or arm not in VALID_ARMS for arm in search_arms)
        or len({arm for arm in search_arms if isinstance(arm, str)}) != len(search_arms)
        or "vulnerable" not in search_arms
    ):
        raise SystemExit(
            f"{path}: ci.search_arms must include vulnerable and contain unique valid arms"
        )
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
    for key in ("replay_max_sessions", "replay_timeout_seconds"):
        value = ci.get(key, 2 if key == "replay_max_sessions" else 1800)
        if not isinstance(value, int) or value <= 0:
            raise SystemExit(f"{path}: ci.{key} must be a positive integer")
    planned_replay_sessions = 1 + 2 * len(sample_paths)
    if state == "runnable" and ci.get("replay_max_sessions", 2) < planned_replay_sessions:
        raise SystemExit(
            f"{path}: ci.replay_max_sessions is below the planned aggregate "
            f"replay count ({planned_replay_sessions})"
        )

    return {
        "id": case["id"],
        "dir": str(path.parent.relative_to(ROOT)),
        "software": case["software"],
        "ci_status": state,
        "ci_reason": ci.get("reason", ""),
        "image_prefix": prefix,
        "vulnerable_version": case["arms"]["vulnerable"]["version"],
        "control_version": case["arms"]["control"]["version"],
        "oracle_assertion": case["oracle"]["assertion"],
        "oracle_evidence": case["oracle"]["evidence"],
        "horizon_ms": case["run"]["horizon_ms"],
        "ram_mib": case["run"]["ram_mib"],
        "seed": case["search"]["seed"],
        "workers": case["search"]["workers"],
        "actions": case["search"]["actions"],
        "wall_minutes": case["search"]["wall_minutes"],
        "search_timeout_minutes": search_timeout_minutes,
        "executions": executions,
        "knobs": " ".join(
            f"{key}={value}" for key, value in case.get("run", {}).get("knobs", {}).items()
        ),
        "sample_paths": sample_paths,
        "replay_max_sessions": case.get("ci", {}).get("replay_max_sessions", 2),
        "replay_timeout_seconds": case.get("ci", {}).get("replay_timeout_seconds", 1800),
        "search_arms": search_arms,
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
    parser.add_argument(
        "--search-matrix",
        action="store_true",
        help="emit a case-by-arm GitHub matrix JSON object",
    )
    parser.add_argument("--check", action="store_true", help="validate manifests without output")
    args = parser.parse_args()
    entries = [validate(path, case) for path, case in cases()]
    if sum(bool(value) for value in (args.matrix, args.runnable_matrix, args.search_matrix)) > 1:
        parser.error("choose only one matrix mode")
    if args.matrix or args.runnable_matrix:
        selected = entries if args.matrix else [entry for entry in entries if entry["ci_status"] == "runnable"]
        json.dump({"include": selected}, sys.stdout, separators=(",", ":"))
        sys.stdout.write("\n")
    elif args.search_matrix:
        search_entries = []
        for entry in entries:
            if entry["ci_status"] != "runnable":
                continue
            for arm in entry["search_arms"]:
                search_entries.append(
                    {
                        **entry,
                        "arm": arm,
                        "workload_version": entry[f"{arm}_version"],
                    }
                )
        json.dump({"include": search_entries}, sys.stdout, separators=(",", ":"))
        sys.stdout.write("\n")
    elif not args.check:
        for entry in entries:
            state = entry["ci_status"]
            suffix = f": {entry['ci_reason']}" if state == "deferred" else ""
            print(f"{entry['id']}\t{state}{suffix}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
