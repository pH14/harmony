#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Partial CLI hardware gate: exact original/fork/cold/split identity.

Exec/probe acceptance remains in historical-investigate.sh, unchanged.
Every CLI invocation below is a separate process. Missing or truncated evidence
is a failure, never a reason to compare only assertion IDs.
"""
import argparse
import copy
import json
import os
from pathlib import Path
import re
import subprocess


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def identity(point):
    moment = point.get("virtual_time")
    digest = point.get("state_hash")
    require(type(moment) is int and moment >= 0, "missing exact endpoint time")
    require(isinstance(digest, str) and re.fullmatch(r"[0-9a-f]{64}", digest),
            "missing direct engine digest")
    return moment, digest


def complete_events(events, moment):
    require(isinstance(events, list), "events must be an array")
    for position, event in enumerate(events):
        require(type(event.get("position")) is int and event["position"] == position,
                f"incomplete event stream at {position}")
        require(type(event.get("virtual_time")) is int
                and 0 <= event["virtual_time"] <= moment, "event outside endpoint")
        require(type(event.get("event")) is int and 0 <= event["event"] <= 0xffffffff,
                "invalid event identifier")
        payload = event.get("payload")
        require(isinstance(payload, str) and re.fullmatch(r"(?:[0-9a-f]{2})*", payload),
                "invalid raw event payload")
    return events


def original_evidence(document):
    require(document.get("format") == "harmony-replay-events-v1", "missing replay sidecar format")
    require(document.get("run") == 1, "wrong original replay ordinal")
    require(document.get("state_hash_encoding") == "engine_digest", "legacy original digest")
    require(document.get("truncated", False) is False, "truncated original stream")
    moment, _ = identity(document)
    return complete_events(document.get("events"), moment)


def inspected_events(view, point):
    require(view.get("format") == "harmony-inspect-evidence-v1", "wrong evidence view")
    require(view.get("view") == "events" and view.get("moment") == point.get("moment"),
            "evidence names another endpoint")
    require(view.get("truncated") is False and view.get("offset") == 0,
            "truncated or offset continuation stream")
    require(view.get("shown") == view.get("matched") == len(view.get("lines", [])),
            "incomplete rendered evidence")
    return complete_events(json.loads("\n".join(view["lines"])), identity(point)[0])


def same_endpoint(original, events, point, view):
    require(identity(original) == identity(point),
            f"endpoint differs: original {identity(original)}, actual {identity(point)}")
    actual = inspected_events(view, point)
    require(actual == events, "complete raw SDK event streams differ")


def retained_identity(summary, point):
    require(summary.get("state_hash_encoding") == "engine_digest", "legacy continuation digest")
    require(summary.get("moment") == point.get("moment") and identity(summary) == identity(point),
            "reply and retained moment name different endpoints")


def self_test():
    original = {"format": "harmony-replay-events-v1", "run": 1, "virtual_time": 10,
                "state_hash": "42" * 32, "state_hash_encoding": "engine_digest",
                "events": [{"position": 0, "virtual_time": 9, "event": 7, "payload": "ab"}]}
    point = {"virtual_time": 10, "state_hash": "42" * 32, "moment": "m-2"}
    view = {"format": "harmony-inspect-evidence-v1", "view": "events", "moment": "m-2",
            "truncated": False, "offset": 0, "shown": 1, "matched": 1,
            "lines": [json.dumps(original["events"])]}
    events = original_evidence(original)
    same_endpoint(original, events, point, view)
    retained = {**point, "state_hash_encoding": "engine_digest"}
    retained_identity(retained, point)
    try:
        retained_identity({**retained, "state_hash_encoding": "legacy_sha256_of_digest"}, point)
    except AssertionError:
        pass
    else:
        raise AssertionError("legacy continuation digest escaped")
    mutations = [
        ("time", lambda p, v: p.update(virtual_time=11)),
        ("digest", lambda p, v: p.update(state_hash="43" * 32)),
        ("payload", lambda p, v: v.update(lines=[json.dumps([{**events[0], "payload": "ac"}])])),
        ("missing event", lambda p, v: v.update(lines=["[]"])),
        ("truncation", lambda p, v: v.update(truncated=True)),
        ("wrong moment", lambda p, v: v.update(moment="m-3")),
        ("stream gap", lambda p, v: v.update(lines=[json.dumps([{**events[0], "position": 1}])])),
    ]
    for name, mutate in mutations:
        p, v = copy.deepcopy(point), copy.deepcopy(view)
        mutate(p, v)
        try:
            same_endpoint(original, events, p, v)
        except AssertionError:
            continue
        raise AssertionError(f"negative control escaped: {name}")
    print("identity comparator: positive and eight negative controls passed")


def qualify(root):
    root = root.resolve()
    output = root / "reports" / "candidate-continuation"
    output.mkdir(parents=True, exist_ok=False)
    workspace = output / "workspace"
    cli = str(root / "tools/harmony")
    artifacts = ["--kernel", str(root / "guest/bzImage-faultlab"),
                 "--base-initramfs", str(root / "guest/initramfs.cpio.gz"),
                 "--fault-agent", str(root / "tools/fault-agent")]
    case = json.loads((root / "bugs/historical/postgres-cic-corruption/case.json").read_text())
    horizon = case["run"]["horizon_ms"]
    rewind = horizon * 1000000 * 3

    def invoke(label, args, parse=True):
        with (output / f"{label}.stderr").open("wb") as stderr:
            completed = subprocess.run([cli, *args], cwd=root, stdout=subprocess.PIPE,
                                       stderr=stderr, timeout=1800, check=False)
        (output / f"{label}.json").write_bytes(completed.stdout)
        require(completed.returncode == 0,
                f"{label} failed ({completed.returncode}); see its stderr artifact")
        return json.loads(completed.stdout) if parse else None

    def w(label, *args):
        return invoke(label, ["-w", str(workspace), "--json", *artifacts, *args])

    invoke("replay", ["search", "--package", "faults",
                      "oci-images/pgcic-14.3.oci", "--backend", "consonance", *artifacts,
                      "--replay", case["witness"], "--repeat", "1", "--horizon-ms", str(horizon),
                      "--ram-mib", str(case["run"]["ram_mib"]), "--knobs",
                      "faultlab.churn_rows=20 faultlab.churn_slices=2 faultlab.churn_rounds=1200",
                      "--out", str(workspace)], parse=False)
    original = json.loads((workspace / "replay-1-events.json").read_text())
    events = original_evidence(original)
    report = json.loads((workspace / "report.json").read_text())
    require(case["oracle"]["evidence"] in report["replays"][0]["sometimes"],
            "replay lacks the qualified detector evidence")
    findings = w("findings", "findings")
    require(len(findings["findings"]) == 1, "replay must retain exactly one actual finding")
    require(findings["findings"][0].get("verified") is True, "replay finding is unconfirmed")
    source = w("source-before", "inspect", "bug-1")
    require(source.get("state_hash_encoding") == "engine_digest", "finding digest encoding")
    require(identity(source) == identity(original), "sidecar and finding name different endpoints")
    require(case["oracle"]["assertion"] in findings["findings"][0]["violations"],
            "replay did not reproduce the qualified witness")

    def compare(label, point):
        retained = w(f"{label}-point", "inspect", point["moment"])
        retained_identity(retained, point)
        view = w(f"{label}-events", "inspect", point["moment"], "events", "--limit", "1000000")
        same_endpoint(original, events, point, view)

    zero = w("fork-zero", "fork", "bug-1", "--name", "zero", "--request-id", "zero")
    compare("zero", zero)
    cold = w("fork-cold", "fork", zero["moment"], "--name", "cold")
    compare("cold", cold)
    for branch in ("whole", "split"):
        start = w(f"fork-{branch}", "fork", "bug-1", "--rewind", f"{rewind}ns", "--name", branch)
        require(start["virtual_time"] < original["virtual_time"], "rewind did not precede finding")
        if branch == "split":
            intermediate = w("split-first", "run", branch, "--for", f"{rewind // 2}ns",
                             "--wall-seconds", "600")
            require(start["virtual_time"] < intermediate["virtual_time"] < original["virtual_time"],
                    "split must actually advance and checkpoint before the endpoint")
        end = w(f"{branch}-end", "run", branch, "--for", f"{rewind * 2}ns",
                "--wall-seconds", "600", "--request-id", f"{branch}-end")
        compare(branch, end)
    require(w("source-after", "inspect", "bug-1") == source, "original finding changed")
    summary = {"format": "harmony-cli-continuation-qualification-v1", "status": "passed",
               "virtual_time": original["virtual_time"], "state_hash": original["state_hash"],
               "events": len(events), "comparisons": ["fork-zero", "cold-clone", "whole", "split"],
               "scope": "exact original/fork/cold/split CLI identity; exec and probes remain pending"}
    (output / "result.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as stream:
            stream.write("\nCLI original/fork/cold/split identity passed: "
                         f"{len(events)} exact SDK events at {original['virtual_time']} ns. "
                         "Exec/probe qualification remains pending.\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    self_test() if args.self_test else qualify(args.root)
