#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Select work inside its real CI job; never claim an unselected test ran."""

import argparse
import os
from pathlib import Path
import subprocess

from ci_scope import SMOKES, selected
from miri_scope import TARGETS, selected_targets
from quality_scope import kani_required


def changed_paths(kind, event, base, before):
    if event not in {"pull_request", "push"} or (kind == "miri" and event != "pull_request"):
        raise ValueError(f"unsupported event for {kind}: {event}")
    if event == "pull_request":
        if not base:
            raise ValueError("pull requests require a base SHA")
        command = ["git", "diff", "--no-renames", "--name-only", "-z", f"{base}...HEAD"]
    elif not before or before == "0" * 40:
        command = ["git", "ls-files", "-z"]
    elif kind == "smoke":
        command = ["git", "diff", "--no-renames", "--name-only", "-z", f"{before}...HEAD"]
    else:
        command = ["git", "diff", "--no-renames", "--name-only", "-z", before, "HEAD"]
    return [path for path in subprocess.check_output(command, text=True).split("\0") if path]


def selection(kind, target, paths):
    paths = list(paths)
    if kind == "smoke":
        if target not in SMOKES:
            raise ValueError(f"unknown smoke: {target}")
        return {"enabled": selected(paths)[target]}
    if kind == "miri":
        if target not in {item["name"] for item in TARGETS}:
            raise ValueError(f"unknown Miri target: {target}")
        match = next((item for item in selected_targets(paths) if item["name"] == target), None)
        return {"enabled": match is not None, "command": match["command"] if match else "",
                "miriflags": match["miriflags"] if match else ""}
    if target:
        raise ValueError(f"{kind} does not accept a target")
    if kind == "kani":
        return {"enabled": kani_required(paths)}
    if kind == "public_api":
        return {"enabled": selected(paths)["public_api"]}
    raise ValueError(f"unknown check kind: {kind}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kind", choices=("smoke", "kani", "public_api", "miri"), required=True)
    parser.add_argument("--target", default="")
    args = parser.parse_args()
    paths = changed_paths(args.kind, os.environ.get("EVENT_NAME", ""),
                          os.environ.get("BASE_SHA", ""), os.environ.get("BEFORE_SHA", ""))
    result = selection(args.kind, args.target, paths)
    label = f"{args.kind} {args.target}".strip()
    status = "Selected by changed files; test steps follow." if result["enabled"] else "Not applicable: no relevant files changed. Test steps were not run."
    with Path(os.environ["GITHUB_STEP_SUMMARY"]).open("a") as summary:
        summary.write(f"### {label}\n\n{status}\n")
    for key, value in result.items():
        print(f"{key}={str(value).lower() if isinstance(value, bool) else value}")


if __name__ == "__main__":
    main()
