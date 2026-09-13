#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Render the historical-bug roster table into workloads/bugs/historical/README.md.

Reads every ``workloads/bugs/historical/*/case.json`` and, optionally, a directory of
report files produced by ``.github/workflows/historical-bugs.yml``. Report
directories are named ``<case id>.<arm>.<mode>`` and hold the ``report.json``
the CLI wrote, so a report attaches to a case without any extra bookkeeping.

``--check`` compares the rendered table with what is committed and exits
non-zero when they differ, so quality CI can enforce the table later.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

BEGIN = "<!-- render-historical-bugs:begin -->"
END = "<!-- render-historical-bugs:end -->"

COLUMNS = (
    "bug",
    "versions",
    "status",
    "CI",
    "discovery",
    "latest replay",
    "latest control",
    "executions to first hit",
    "replay command",
)

# Report directory names carry the arm and the mode; the current panel's fresh
# discovery replay feeds the replay columns and a search feeds the executions
# column.
REPLAY_MODES = ("discovery",)


def repo_root() -> Path:
    return Path(__file__).resolve().parent.parent


def load_cases(root: Path) -> list[dict]:
    cases = []
    for path in sorted(root.glob("workloads/bugs/historical/*/case.json")):
        case = json.loads(path.read_text())
        case["_dir"] = path.parent
        cases.append(case)
    return cases


def load_reports(reports_dir: Path | None) -> dict[tuple[str, str, str], dict]:
    """Newest report per (case id, arm, mode), keyed by report file mtime."""
    found: dict[tuple[str, str, str], tuple[float, dict]] = {}
    if reports_dir is None:
        return {}
    for path in sorted(reports_dir.rglob("report.json")):
        parts = path.parent.name.split(".")
        if len(parts) < 3:
            continue
        mode = parts[-1]
        arm = parts[-2]
        case_id = ".".join(parts[:-2])
        try:
            report = json.loads(path.read_text())
        except (OSError, json.JSONDecodeError):
            continue
        key = (case_id, arm, mode)
        stamp = path.stat().st_mtime
        if key not in found or stamp >= found[key][0]:
            found[key] = (stamp, report)
    return {key: report for key, (_, report) in found.items()}


def replay_outcome(reports: dict, case_id: str, arm: str) -> str:
    """The newest replay verdict for one arm, as plain words."""
    for mode in reversed(REPLAY_MODES):
        report = reports.get((case_id, arm, mode))
        if report is None:
            continue
        replays = report.get("replays") or []
        if not replays:
            return "no replays"
        hits = sum(1 for replay in replays if replay.get("bug"))
        word = "bug found" if hits else "silent"
        return f"{word} ({hits}/{len(replays)}, {mode})"
    return "—"


def first_hit(reports: dict, case_id: str) -> str:
    report = reports.get((case_id, "vulnerable", "search"))
    if report is None:
        return "—"
    if not report.get("bug_found"):
        return "not found"
    execution = report.get("first_bug_execution")
    return "—" if execution is None else str(execution)


def replay_command(case: dict) -> str:
    """Show the command shape for a reproducer from the current run."""
    if case.get("ci", {}).get("status", "runnable") != "runnable":
        return "—"
    version = case.get("arms", {}).get("vulnerable", {}).get("version", "?")
    run = case.get("run", {})
    knobs = " ".join(f"{key}={value}" for key, value in run.get("knobs", {}).items())
    command = [
        "harmony search --package faults",
        f"IMAGE-{version}.oci",
        "--backend consonance",
        f"--kernel bzImage-{case.get('kernel_profile', '?')}",
        "--base-initramfs initramfs.cpio.gz",
        "--fault-agent fault-agent --replay OUT/first-bug-input.json --repeat 1",
        f"--ram-mib {run.get('ram_mib', '?')}",
    ]
    if knobs:
        command.append(f'--knobs "{knobs}"')
    command.append("--out OUT")
    return f"`{' '.join(command)}`"


def render(cases: list[dict], reports: dict) -> str:
    lines = [
        "| " + " | ".join(COLUMNS) + " |",
        "|" + "---|" * len(COLUMNS),
    ]
    for case in cases:
        case_id = case["id"]
        versions = case.get("versions", {})
        arms = case.get("arms", {})
        vulnerable = arms.get("vulnerable", {}).get("version", versions.get("affected", "?"))
        fixed = arms.get("control", {}).get("version", versions.get("fixed", "?"))
        row = (
            f"[{case_id}]({case_id}/README.md)",
            f"{vulnerable} / {fixed}",
            case.get("status", "?"),
            case.get("ci", {}).get("status", "runnable")
            + (
                f": {case['ci']['reason']}"
                if case.get("ci", {}).get("status") == "deferred"
                else ""
            ),
            case.get("discovery_mode", "?"),
            replay_outcome(reports, case_id, "vulnerable"),
            replay_outcome(reports, case_id, "control"),
            first_hit(reports, case_id),
            replay_command(case),
        )
        lines.append("| " + " | ".join(row) + " |")
    return "\n".join(lines)


def splice(readme: str, table: str) -> str:
    start = readme.find(BEGIN)
    end = readme.find(END)
    if start == -1 or end == -1 or end < start:
        raise SystemExit(
            f"workloads/bugs/historical/README.md is missing the {BEGIN} / {END} markers"
        )
    head = readme[: start + len(BEGIN)]
    tail = readme[end:]
    return f"{head}\n{table}\n{tail}"


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--reports",
        type=Path,
        default=None,
        help="directory holding <case>.<arm>.<mode>/report.json trees",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit non-zero when the committed table is stale, and write nothing",
    )
    args = parser.parse_args()

    root = repo_root()
    readme_path = root / "workloads/bugs/historical/README.md"
    cases = load_cases(root)
    if not cases:
        raise SystemExit("no workloads/bugs/historical/*/case.json found")
    reports = load_reports(args.reports)

    readme = readme_path.read_text()
    rendered = splice(readme, render(cases, reports))
    if args.check:
        if rendered != readme:
            print(
                "workloads/bugs/historical/README.md roster is stale; "
                "run scripts/render-historical-bugs.py",
                file=sys.stderr,
            )
            return 1
        return 0
    if rendered != readme:
        readme_path.write_text(rendered)
        print(f"wrote {readme_path}")
    else:
        print(f"{readme_path} already current")
    return 0


if __name__ == "__main__":
    sys.exit(main())
