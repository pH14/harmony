#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Reduce a campaign report to the frozen SMB progress-AUC scorecard fields."""

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any


PROGRESS_PER_LEVEL = 256
LEVELS_PER_WORLD = 4


def progress_ordinal(progress: dict[str, Any]) -> int:
    """Map a mechanical SMB progress tuple to a monotonically ordered integer."""
    world = require_int(progress, "world")
    level = require_int(progress, "level")
    offset = require_int(progress, "progress")
    if not 0 <= offset < PROGRESS_PER_LEVEL:
        raise ValueError(f"progress must be in [0, {PROGRESS_PER_LEVEL}): {offset}")
    return ((world * LEVELS_PER_WORLD) + level) * PROGRESS_PER_LEVEL + offset


def require_int(value: dict[str, Any], key: str) -> int:
    item = value.get(key)
    if isinstance(item, bool) or not isinstance(item, int):
        raise ValueError(f"{key} must be an integer")
    return item


def reduce_report(path: Path) -> dict[str, Any]:
    raw = path.read_bytes()
    report = json.loads(raw)
    archive = report.get("archive")
    if not isinstance(archive, dict):
        raise ValueError("report archive must be an object")

    entries = archive.get("entries")
    if not isinstance(entries, list) or not entries:
        raise ValueError("report archive entries must be a non-empty list")
    roots = [entry for entry in entries if entry.get("id") == 0]
    if len(roots) != 1 or not isinstance(roots[0].get("key"), dict):
        raise ValueError("report must contain exactly one root entry with id 0")
    origin_ordinal = progress_ordinal(roots[0]["key"])

    curve = archive.get("progress_curve")
    if not isinstance(curve, list) or not curve:
        raise ValueError("report progress_curve must be a non-empty list")

    auc = 0
    previous_execution = 0
    previous_ordinal = origin_ordinal
    samples: list[dict[str, int]] = []
    for sample in curve:
        if not isinstance(sample, dict) or not isinstance(sample.get("progress"), dict):
            raise ValueError("every progress_curve sample must contain progress")
        execution = require_int(sample, "executions")
        ordinal = progress_ordinal(sample["progress"])
        if execution <= previous_execution:
            raise ValueError("progress_curve executions must increase strictly")
        if ordinal < previous_ordinal:
            raise ValueError("progress_curve must be monotonic")
        normalized = ordinal - origin_ordinal
        auc += (execution - previous_execution) * normalized
        samples.append({"executions": execution, "normalized_progress": normalized})
        previous_execution = execution
        previous_ordinal = ordinal

    completed = require_int(report, "executions_completed")
    if previous_execution != completed:
        raise ValueError(
            "final progress_curve execution must equal executions_completed: "
            f"{previous_execution} != {completed}"
        )

    return {
        "format": "smb-progress-auc-v1",
        "source": str(path),
        "source_sha256": hashlib.sha256(raw).hexdigest(),
        "campaign_seed": require_int(report, "campaign_seed"),
        "executions_completed": completed,
        "victories": require_int(report, "victories"),
        "origin_ordinal": origin_ordinal,
        "final_normalized_progress": samples[-1]["normalized_progress"],
        "progress_auc": auc,
        "curve_samples": samples,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    print(json.dumps(reduce_report(args.report), sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
