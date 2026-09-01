#!/usr/bin/env -S uv run --script
# SPDX-License-Identifier: AGPL-3.0-or-later
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy"]
# ///
"""Fit per-exit-class V-time durations from calibration logs.

A calibration log is the per-event wall-clock series emitted by
`hvf_postgres_m3 ... [calibration-log]` or the x86 boot lane under
`X2_CALIBRATION_LOG`. Each line is:

    calib event=<index> class=<label> vns_after=<vns> wall_ns=<wall>

Two estimates are reported per class:

- the median wall-clock delta attributed to events of that class (the delta
  between an event and its predecessor lands on the later event's class);
- an ordinary-least-squares fit of total wall time against per-class event
  counts across all input logs (meaningful once the number of logs is at
  least the number of classes).

Paravirtual-MMIO events whose V-time delta equals --tick-vns are reported
separately as `tick`. Idle events are excluded from suggestions: their V-time
advance is the jump to the next deadline, not a per-exit constant.
"""

from __future__ import annotations

import argparse
import re
import statistics
import sys
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np

ROW_RE = re.compile(
    r"^calib event=(?P<event>\d+) class=(?P<class>\w+) "
    r"vns_after=(?P<vns>\d+) wall_ns=(?P<wall>\d+)$"
)

NON_CONSTANT_CLASSES = {"idle", "terminal"}


def parse_log(path: Path) -> list[tuple[str, int, int]]:
    """Return (class, vns_after, wall_ns) rows in event order."""
    rows = []
    for line_number, line in enumerate(path.read_text().splitlines(), start=1):
        match = ROW_RE.match(line)
        if match is None:
            raise SystemExit(f"{path}:{line_number}: unrecognized row: {line!r}")
        rows.append(
            (match["class"], int(match["vns"]), int(match["wall"]))
        )
    if not rows:
        raise SystemExit(f"{path}: no calibration rows")
    return rows


def subclassify(rows: list[tuple[str, int, int]], tick_vns: int) -> list[tuple[str, int, int]]:
    """Split pv_mmio events that advanced by exactly tick_vns into `tick`."""
    out = []
    previous_vns = 0
    for cls, vns, wall in rows:
        if cls == "pv_mmio" and vns - previous_vns == tick_vns:
            cls = "tick"
        out.append((cls, vns, wall))
        previous_vns = vns
    return out


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("logs", nargs="+", type=Path)
    parser.add_argument(
        "--tick-vns",
        type=int,
        default=1_000_000,
        help="execution-tick V-ns constant used to split ticks out of pv_mmio",
    )
    args = parser.parse_args()

    deltas: dict[str, list[int]] = defaultdict(list)
    totals: list[tuple[Counter, int, int, Path]] = []
    for path in args.logs:
        rows = subclassify(parse_log(path), args.tick_vns)
        counts = Counter(cls for cls, _, _ in rows)
        previous_wall = 0
        for cls, _, wall in rows:
            deltas[cls].append(wall - previous_wall)
            previous_wall = wall
        last_vns = rows[-1][1]
        last_wall = rows[-1][2]
        totals.append((counts, last_wall, last_vns, path))
        print(
            f"{path}: events={len(rows)} wall_s={last_wall / 1e9:.2f} "
            f"virtual_s={last_vns / 1e9:.4f} ratio={last_vns / last_wall:.5f}"
        )

    classes = sorted(cls for cls in deltas if cls not in NON_CONSTANT_CLASSES)
    print(f"\n{'class':<14}{'count':>10}{'median_ns':>12}{'mean_ns':>12}{'p90_ns':>12}")
    for cls in sorted(deltas):
        values = sorted(deltas[cls])
        note = "  (no per-exit constant)" if cls in NON_CONSTANT_CLASSES else ""
        print(
            f"{cls:<14}{len(values):>10}{int(statistics.median(values)):>12}"
            f"{int(statistics.fmean(values)):>12}"
            f"{values[int(0.9 * (len(values) - 1))]:>12}{note}"
        )

    if len(totals) >= len(classes):
        design = np.array(
            [[counts.get(cls, 0) for cls in classes] for counts, _, _, _ in totals],
            dtype=float,
        )
        wall = np.array([wall for _, wall, _, _ in totals], dtype=float)
        fitted, residuals, rank, _ = np.linalg.lstsq(design, wall, rcond=None)
        print(f"\nleast squares over {len(totals)} logs (rank {rank}):")
        for cls, value in zip(classes, fitted):
            print(f"  {cls:<14}{value:>14.1f} ns/event")
        if residuals.size:
            print(f"  residual RMS: {float(np.sqrt(residuals[0] / len(totals))):.3e} ns")
    else:
        print(
            f"\nleast squares skipped: {len(totals)} logs < {len(classes)} classes "
            "(median deltas above are the quick estimate)"
        )


if __name__ == "__main__":
    sys.exit(main())
