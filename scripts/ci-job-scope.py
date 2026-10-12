#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Select work inside its real CI job; never claim an unselected test ran."""

import argparse
import json
import os
from pathlib import Path
import subprocess

from ci_contract import MIRI_OWNERS, WORKFLOWS
from ci_scope import SCENARIOS, selected
from miri_scope import TARGETS, selected_targets
from quality_scope import kani_required
from rust_scope import rust_checks_required


FULL_RUN_KINDS = ("harmony_nes", "harmony_languages", "rust_checks")


def changed_paths(kind, event, base, before):
    if kind in FULL_RUN_KINDS and event in ("workflow_dispatch", "schedule"):
        command = ["git", "ls-files", "-z"]
    elif event not in {"pull_request", "push"}:
        raise ValueError(f"unsupported event for {kind}: {event}")
    elif event == "pull_request":
        if not base:
            raise ValueError("pull requests require a base SHA")
        command = ["git", "diff", "--no-renames", "--name-only", "-z", f"{base}...HEAD"]
    elif not before or before == "0" * 40:
        command = ["git", "ls-files", "-z"]
    elif kind in SCENARIOS:
        command = ["git", "diff", "--no-renames", "--name-only", "-z", f"{before}...HEAD"]
    else:
        command = ["git", "diff", "--no-renames", "--name-only", "-z", before, "HEAD"]
    try:
        output = subprocess.check_output(command, text=True)
    except subprocess.CalledProcessError:
        if kind != "rust_checks":
            raise
        output = subprocess.check_output(["git", "ls-files", "-z"], text=True)
    return [path for path in output.split("\0") if path]


def selection(kind, target, paths):
    paths = list(paths)
    if kind in SCENARIOS:
        if target:
            raise ValueError(f"{kind} does not accept a target")
        return {"enabled": selected(paths)[kind]}
    if kind == "miri":
        if target not in {item["name"] for item in TARGETS}:
            raise ValueError(f"unknown Miri target: {target}")
        match = next((item for item in selected_targets(paths) if item["name"] == target), None)
        return {"enabled": match is not None, "command": match["command"] if match else "",
                "miriflags": match["miriflags"] if match else ""}
    if kind == "miri_matrix":
        names = {name for name, owner in MIRI_OWNERS.items() if owner == target}
        if not names:
            raise ValueError(f"no Miri targets are owned by {target!r}")
        include = [item for item in selected_targets(paths) if item["name"] in names]
        return {"enabled": bool(include),
                "matrix": json.dumps({"include": include}, separators=(",", ":"))}
    if kind == "rust_checks":
        workflows = {Path(workflow.path).name for workflow in WORKFLOWS
                     if any(job.select == "rust_checks" for job in workflow.jobs)}
        if target not in workflows:
            raise ValueError(f"no workflow selects rust_checks as {target!r}")
        return {"enabled": rust_checks_required(paths, target)}
    if target:
        raise ValueError(f"{kind} does not accept a target")
    if kind == "kani":
        return {"enabled": kani_required(paths)}
    raise ValueError(f"unknown check kind: {kind}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", choices=(*SCENARIOS, "kani", "miri", "miri_matrix", "rust_checks"), required=True)
    parser.add_argument("--target", default="")
    args = parser.parse_args()
    paths = changed_paths(args.kind, os.environ.get("EVENT_NAME", ""),
                          os.environ.get("BASE_SHA", ""), os.environ.get("BEFORE_SHA", ""))
    result = selection(args.kind, args.target, paths)
    label = f"{args.kind} {args.target}".strip()
    status = "Selected by changed files; test steps follow." if result["enabled"] else "Not applicable: no relevant files changed. Test steps were not run."
    if os.environ.get("EVENT_NAME") == "workflow_dispatch" and result["enabled"]:
        status = "Selected by manual dispatch over the tracked source tree; test steps follow."
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as summary:
        summary.write(f"### {label}\n\n{status}\n")
    for key, value in result.items():
        print(f"{key}={str(value).lower() if isinstance(value, bool) else value}")


if __name__ == "__main__":
    main()
