#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""CLI hardware gate for exact continuation and command identity.

Every CLI invocation below is a separate process. Missing or truncated evidence
is a failure, never a reason to compare only assertion IDs.
"""
import argparse
import copy
import importlib.util
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


def command_record(point):
    command = point.get("command")
    require(isinstance(command, dict), "result has no retained command record")
    invocation = command.get("invocation")
    require(isinstance(invocation, dict), "command has no invocation")
    require(type(invocation.get("engine_id")) is int and invocation["engine_id"] >= 0,
            "command has no valid engine identity")
    require(isinstance(invocation.get("request_id"), str) and invocation["request_id"],
            "command has no request identity")
    require(isinstance(invocation.get("argv"), list) and invocation["argv"]
            and all(isinstance(arg, str) for arg in invocation["argv"]),
            "command has no argv")
    require(isinstance(command.get("evidence"), str) and command["evidence"],
            "command has no evidence id")
    return command


def command_pending(point, request_id=None, argv=None):
    command = command_record(point)
    invocation = command["invocation"]
    require(command.get("completion") == "pending", "command is not pending")
    if request_id is not None:
        require(invocation["request_id"] == request_id, "pending command request id differs")
    if argv is not None:
        require(invocation["argv"] == argv, "pending command argv differs")
    require(command["evidence"] in point.get("evidence", []),
            "pending command evidence is absent from the reply")
    return command


def command_exited(point, status):
    command = command_record(point)
    invocation = command["invocation"]
    completion = command.get("completion")
    require(isinstance(completion, dict) and set(completion) == {"exited"},
            "command is not an exited command")
    exited = completion["exited"]
    require(isinstance(exited, dict), "exited command has no completion payload")
    require(type(exited.get("status")) is int and exited["status"] == status,
            f"command exited with an unexpected status (wanted {status})")
    require(type(exited.get("virtual_time")) is int
            and 0 <= exited["virtual_time"] <= point.get("virtual_time", -1),
            "command completion is outside its retained endpoint")
    require(command["evidence"] in point.get("evidence", []),
            "exited command evidence is absent from the reply")
    return command, exited["virtual_time"]


def validate_command_evidence(summary, view, command, expected_output=None):
    require(summary.get("format") == "harmony-inspect-v1", "wrong command summary format")
    require(view.get("format") == "harmony-inspect-evidence-v1"
            and view.get("view") == "command", "wrong command evidence view")
    require(view.get("moment") == summary.get("moment"),
            "command evidence names another endpoint")
    evidence = [item for item in summary.get("evidence", [])
                if item.get("kind") == "command"]
    require(len(evidence) == 1, "endpoint does not have exactly one command evidence record")
    record = evidence[0]
    require(type(record.get("bytes")) is int and record["bytes"] >= 0,
            "command evidence has no valid byte count")
    require(record.get("evidence") == command.get("evidence")
            and view.get("evidence") == command.get("evidence"),
            "command evidence is not bound to the command record")
    require(isinstance(record.get("sha256"), str)
            and re.fullmatch(r"[0-9a-f]{64}", record["sha256"]),
            "command evidence has no raw byte digest")
    require(record.get("truncated") is False and view.get("truncated") is False,
            "command evidence is truncated")
    require(view.get("offset") == 0 and isinstance(view.get("lines"), list)
            and view.get("shown") == view.get("matched") == len(view["lines"]),
            "command evidence view is incomplete")
    require(all(isinstance(line, str) for line in view["lines"]),
            "command evidence contains a non-text line")
    if expected_output is not None:
        require(expected_output in "\n".join(view["lines"]),
                "command evidence is missing its output witness")
    return view["lines"]


def same_command_output(first, second):
    require(first.get("bytes") == second.get("bytes"), "command output byte counts differ")
    require(first.get("sha256") == second.get("sha256"), "raw command output digests differ")


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

    argv = ["/opt/harmony/busybox", "sh", "-c", "exit 37"]
    pending = {
        "format": "harmony-inspect-v1",
        "moment": "m-2",
        "virtual_time": 10,
        "state_hash": "42" * 32,
        "state_hash_encoding": "engine_digest",
        "evidence": ["ev-3"],
        "command": {
            "invocation": {"engine_id": 7, "request_id": "pending-exec", "argv": argv},
            "completion": "pending",
            "evidence": "ev-3",
        },
    }
    command_summary = {
        "format": "harmony-inspect-v1",
        "moment": "m-2",
        "virtual_time": 10,
        "state_hash": "42" * 32,
        "state_hash_encoding": "engine_digest",
        "evidence": [{"evidence": "ev-3", "kind": "command", "bytes": 8, "sha256": "55" * 32,
                       "truncated": False}],
    }
    command_view = {
        "format": "harmony-inspect-evidence-v1", "moment": "m-2", "view": "command",
        "evidence": "ev-3", "offset": 0, "shown": 1, "matched": 1,
        "truncated": False, "lines": ["WITNESS"],
    }
    command_pending(pending, "pending-exec", argv)
    validate_command_evidence(command_summary, command_view, pending["command"], "WITNESS")
    exited = copy.deepcopy(pending)
    exited["command"]["completion"] = {"exited": {"status": 37, "virtual_time": 10}}
    command_exited(exited, 37)
    command_mutations = [
        ("wrong pending status", lambda p, s, v: p["command"].update(
            completion={"exited": {"status": 37, "virtual_time": 10}})),
        ("wrong exit status", lambda p, s, v: p["command"].update(
            completion={"exited": {"status": 38, "virtual_time": 10}})),
        ("completion after endpoint", lambda p, s, v: p["command"].update(
            completion={"exited": {"status": 37, "virtual_time": 11}})),
        ("invalid command byte count", lambda p, s, v: s["evidence"][0].update(bytes=-1)),
        ("wrong command output", lambda p, s, v: v.update(lines=["OTHER"])),
        ("wrong command endpoint", lambda p, s, v: v.update(moment="m-3")),
        ("wrong command evidence", lambda p, s, v: s.update(
            evidence=[{"evidence": "ev-4", "kind": "command", "bytes": 8, "sha256": "55" * 32,
                       "truncated": False}])),
    ]
    for name, mutate in command_mutations:
        p, s, v = copy.deepcopy(pending), copy.deepcopy(command_summary), copy.deepcopy(command_view)
        mutate(p, s, v)
        try:
            if name == "wrong pending status":
                command_pending(p, "pending-exec", argv)
            elif name in ("wrong exit status", "completion after endpoint"):
                command_exited(p, 37)
            else:
                validate_command_evidence(s, v, p["command"], "WITNESS")
        except AssertionError:
            continue
        raise AssertionError(f"command validator negative control escaped: {name}")
    raw = command_summary["evidence"][0]
    same_command_output(raw, raw)
    try:
        same_command_output(raw, {**raw, "sha256": "56" * 32})
    except AssertionError:
        pass
    else:
        raise AssertionError("different raw command bytes escaped")
    print("identity and command comparators: positive and sixteen negative controls passed")


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

    def invoke_expect_failure(label, args):
        with (output / f"{label}.stderr").open("wb") as stderr:
            completed = subprocess.run([cli, *args], cwd=root, stdout=subprocess.PIPE,
                                       stderr=stderr, timeout=1800, check=False)
        (output / f"{label}.json").write_bytes(completed.stdout)
        require(completed.returncode != 0, f"{label} unexpectedly succeeded")
        return completed

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

    def command_endpoint(label, point, expected_output=None):
        summary = w(f"{label}-summary", "inspect", point["moment"])
        retained_identity(summary, point)
        command = command_record(point)
        require(summary.get("command") == command, "reply and retained command record differ")
        view = w(f"{label}-command", "inspect", point["moment"], "command",
                 "--limit", "1000000")
        lines = validate_command_evidence(summary, view, command, expected_output)
        return summary, view, lines

    def command_pair(label, first, second, expected_output=None):
        require(identity(first) == identity(second),
                f"command endpoints differ at {label}: {identity(first)} vs {identity(second)}")
        first_command = command_record(first)
        second_command = command_record(second)
        require(first_command["invocation"] == second_command["invocation"],
                f"command invocations differ at {label}")
        require(first_command["completion"] == second_command["completion"],
                f"command completions differ at {label}")
        first_summary, first_view, first_lines = command_endpoint(
            f"{label}-first", first, expected_output)
        second_summary, second_view, second_lines = command_endpoint(
            f"{label}-second", second, expected_output)
        first_events = inspected_events(
            w(f"{label}-first-events", "inspect", first["moment"], "events",
              "--limit", "1000000"), first)
        second_events = inspected_events(
            w(f"{label}-second-events", "inspect", second["moment"], "events",
              "--limit", "1000000"), second)
        require(first_events == second_events,
                f"complete raw SDK event streams differ at {label}")
        first_command_evidence = next(
            item for item in first_summary["evidence"] if item.get("kind") == "command")
        second_command_evidence = next(
            item for item in second_summary["evidence"] if item.get("kind") == "command")
        same_command_output(first_command_evidence, second_command_evidence)
        require(first_lines == second_lines, f"command output differs at {label}")
        return first_summary, first_view, first_lines, second_summary, second_view, second_lines

    zero_head = w("zero-head-before-exec", "inspect", "zero@head")
    retained_identity(zero_head, zero)
    pending_script = (
        r"printf '\120\105\116\104\111\116\107\137\105\130\105\103\137"
        r"\127\111\124\116\105\123\123\012'; exit 37"
    )
    pending_output = "PENDING_EXEC_WITNESS"
    require(pending_output not in pending_script,
            "pending command must not echo its literal output witness")
    pending_argv = ["/opt/harmony/busybox", "sh", "-c", pending_script]
    pending = w("pending-exec", "exec", "--at", "zero@head", "--within", "0ns",
                 "--extend", "--request-id", "pending-exec", "--", *pending_argv)
    pending_command = command_pending(pending, "pending-exec", pending_argv)
    require(pending.get("history") == "modified", "pending command did not modify history")
    command_endpoint("pending-exec", pending)
    require(w("zero-after-pending", "inspect", "zero@head") == zero_head,
            "pending probe changed the source branch")

    pending_branch = pending["branch"]
    pending_cold = w("fork-pending-cold", "fork", f"{pending_branch}@head", "--name", "pending-cold",
                      "--request-id", "pending-cold")
    pending_cold_command = command_pending(pending_cold, "pending-exec", pending_argv)
    require(pending_cold_command["invocation"] == pending_command["invocation"],
            "cold pending fork changed command identity")
    require(pending_cold.get("history") == "modified",
            "cold pending fork lost modified history")
    command_pair("pending-fork", pending, pending_cold)

    completed_pair = None
    completed_step = None
    for step in range(1, 21):
        original_step = w(
            f"pending-original-step-{step}", "run", pending_branch, "--for", "1s", "--extend",
            "--wall-seconds", "600", "--request-id", f"pending-original-step-{step}")
        cold_step = w(
            f"pending-cold-step-{step}", "run", "pending-cold", "--for", "1s", "--extend",
            "--wall-seconds", "600", "--request-id", f"pending-cold-step-{step}")
        original_completion = command_record(original_step)["completion"]
        cold_completion = command_record(cold_step)["completion"]
        require(original_completion == cold_completion,
                f"pending branches disagree on completion at step {step}")
        expected_output = pending_output if isinstance(original_completion, dict) else None
        command_pair(
            f"pending-step-{step}", original_step, cold_step, expected_output)
        if original_completion == "pending":
            continue
        command_exited(original_step, 37)
        command_exited(cold_step, 37)
        completed_pair = (original_step, cold_step)
        completed_step = step
        break
    require(completed_pair is not None, "pending command did not complete within 20 one-second steps")
    original_completed, cold_completed = completed_pair
    original_command = command_record(original_completed)
    cold_command = command_record(cold_completed)
    _, original_completion_time = command_exited(original_completed, 37)
    _, cold_completion_time = command_exited(cold_completed, 37)
    require(original_completion_time == cold_completion_time,
            "paired command completion times differ")
    require(original_command["invocation"] == cold_command["invocation"],
            "paired command invocation identity differs after completion")

    retry_branches = w("pending-retry-before-branches", "branches")
    retry_point = w("pending-retry-before-point", "inspect", pending["moment"])
    missing = output / "missing-artifact"
    missing_artifacts = ["--kernel", str(missing / "kernel"),
                         "--base-initramfs", str(missing / "base"),
                         "--fault-agent", str(missing / "agent")]
    retry = invoke(
        "pending-retry",
        ["-w", str(workspace), "--json", *missing_artifacts, "exec", "--at", "zero@head",
         "--within", "0ns", "--extend", "--request-id", "pending-exec", "--", *pending_argv],
    )
    retry_without_flag = copy.deepcopy(retry)
    original_without_flag = copy.deepcopy(pending)
    retry_without_flag.pop("replayed_request", None)
    original_without_flag.pop("replayed_request", None)
    require(retry_without_flag == original_without_flag,
            "cached pending exec reply differs from the original result")
    require(retry.get("replayed_request") is True, "cached retry was not marked as replayed")
    require(w("pending-retry-after-branches", "branches") == retry_branches,
            "cached retry changed branch state")
    require(w("pending-retry-after-point", "inspect", pending["moment"]) == retry_point,
            "cached retry changed retained evidence")

    invoke_expect_failure(
        "pending-changed-argv",
        ["-w", str(workspace), "--json", *missing_artifacts, "exec", "--at", "zero@head",
         "--within", "0ns", "--extend", "--request-id", "pending-exec", "--",
         "/opt/harmony/busybox", "sh", "-c", "exit 38"],
    )
    changed_error = (output / "pending-changed-argv.stderr").read_text()
    require("different arguments" in changed_error,
            f"changed request id failed without a fingerprint error: {changed_error}")
    require(w("pending-changed-after-branches", "branches") == retry_branches,
            "changed request id mutated branch state")

    def finish_command(label, point, status):
        for step in range(21):
            command_endpoint(f"{label}-step-{step}", point)
            if command_record(point)["completion"] != "pending":
                command_exited(point, status)
                return point
            require(step < 20, f"{label} did not complete within its bounded run steps")
            point = w(f"{label}-run-{step}", "run", point["branch"], "--for", "1s",
                      "--extend", "--wall-seconds", "600", "--request-id", f"{label}-run-{step}")

    quick_argv = ["/opt/harmony/busybox", "sh", "-c", "exit 19"]
    quick = w("quick-probe", "exec", "--at", "zero@head", "--within", "1s", "--extend",
               "--wall-seconds", "600", "--request-id", "quick-exec", "--", *quick_argv)
    quick = finish_command("quick-probe", quick, 19)
    command_exited(quick, 19)
    require(quick["branch"] not in {zero["branch"], pending_branch, "pending-cold"},
            "quick --at probe reused an existing branch")
    require(quick.get("history") == "modified", "quick probe did not modify history")
    command_endpoint("quick-probe", quick)
    require(w("zero-after-quick", "inspect", "zero@head") == zero_head,
            "quick probe changed the source branch")

    replacement_argv = ["/opt/harmony/busybox", "sh", "-c", "exit 23"]
    replacement = w("replacement", "exec", quick["branch"], "--within", "1s", "--extend",
                    "--wall-seconds", "600", "--request-id", "replacement-exec", "--", *replacement_argv)
    replacement = finish_command("replacement", replacement, 23)
    replacement_command, _ = command_exited(replacement, 23)
    require(replacement["branch"] == quick["branch"],
            "positional exec did not advance the completed branch")
    require(replacement_command["invocation"]["request_id"] == "replacement-exec"
            and replacement_command["invocation"]["argv"] == replacement_argv,
            "replacement command invocation was not recorded")
    command_endpoint("replacement", replacement)
    old_quick = w("quick-old-after-replacement", "inspect", quick["moment"])
    retained_identity(old_quick, quick)
    command_endpoint("quick-old-after-replacement", quick)
    require(w("zero-after-replacement", "inspect", "zero@head") == zero_head,
            "replacement probe changed the source branch")

    require(w("source-after-commands", "inspect", "bug-1") == source, "original finding changed during commands")

    spec = importlib.util.spec_from_file_location(
        "journal_interruption", Path(__file__).with_name("cli-journal-interruption.py"))
    require(spec is not None and spec.loader is not None, "missing interruption diagnostic")
    journal = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(journal)
    interruption_env = dict(os.environ, HARMONY_CWD=str(root),
                            HARMONY_KERNEL=str(root / "guest/bzImage-faultlab"),
                            HARMONY_BASE_INITRAMFS=str(root / "guest/initramfs.cpio.gz"),
                            HARMONY_FAULT_AGENT=str(root / "tools/fault-agent"))
    source_bytes = journal._source_tree_snapshot(workspace)
    interruption = journal.qualify(cli, workspace, "zero", output / "journal-interruption",
                                   interruption_env)
    phases = interruption["phases"]
    recovered = {phase: data["pending_reply"] for phase, data in phases.items()}

    def recovered_invoke(phase, label, *args):
        return invoke(f"journal-{phase}-{label}",
                      ["-w", phases[phase]["workspace"], "--json", *artifacts, *args])

    def compare_recovered(label):
        endpoints = []
        for phase, point in recovered.items():
            command = command_record(point)
            summary = recovered_invoke(phase, f"{label}-summary", "inspect", point["moment"])
            retained_identity(summary, point)
            require(summary.get("command") == command, "recovered command differs from reply")
            view = recovered_invoke(phase, f"{label}-command", "inspect", point["moment"],
                                    "command", "--limit", "1000000")
            lines = validate_command_evidence(summary, view, command)
            evidence = next(item for item in summary["evidence"] if item.get("kind") == "command")
            raw = journal._verified_blob(
                Path(phases[phase]["workspace"]) / "blobs" / evidence["sha256"], evidence["sha256"])
            require(evidence["bytes"] == len(raw), "recovered command byte count differs from blob")
            require(lines == journal._rust_lossy_lines(raw),
                    "recovered command view differs from raw blob")
            events_view = recovered_invoke(phase, f"{label}-events", "inspect", point["moment"],
                                           "events", "--limit", "1000000")
            invocation = dict(command["invocation"])
            # Each independent transaction has a different host request id.
            # Engine command identity and every guest-visible field must agree.
            invocation.pop("request_id")
            endpoints.append((identity(point), invocation, command["completion"],
                              inspected_events(events_view, point), lines,
                              evidence["bytes"], evidence["sha256"]))
        require(endpoints[0] == endpoints[1], f"recovered before/after states differ at {label}")

    compare_recovered("pending")
    interruption_steps = None
    for step in range(1, 21):
        for phase, point in recovered.items():
            recovered[phase] = recovered_invoke(
                phase, f"run-{step}", "run", point["branch"], "--for", "1s", "--extend",
                "--wall-seconds", "600", "--request-id", f"journal-cold-step-{step}")
        compare_recovered(f"step-{step}")
        if command_record(recovered["before"])["completion"] == "pending":
            continue
        for point in recovered.values():
            command_exited(point, 37)
        interruption_steps = step
        break
    require(interruption_steps is not None, "recovered commands did not complete in 20 steps")
    require(journal._source_tree_snapshot(workspace) == source_bytes,
            "interruption or cold recovery changed the source workspace")

    summary = {"format": "harmony-cli-continuation-qualification-v1", "status": "passed",
               "virtual_time": original["virtual_time"], "state_hash": original["state_hash"],
               "events": len(events),
               "command_completion_steps": completed_step,
               "command_completion_status": 37,
               "journal_interruption_completion_steps": interruption_steps,
               "journal_interruption_phases": list(phases),
               "comparisons": ["fork-zero", "cold-clone", "whole", "split",
                               "pending-exec/fork-pending-cold", "quick-probe/replacement",
                               "journal-before/after-cold-completion"],
               "scope": "exact continuation, exec/probe identity and shipping process interruption recovery"}
    (output / "result.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as stream:
            stream.write("\nCLI original/fork/cold/split identity passed: "
                         f"{len(events)} exact SDK events at {original['virtual_time']} ns. "
                         f"pending exec completed in {completed_step} one-second step(s); "
                         "exec/probe identity passed.\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    self_test() if args.self_test else qualify(args.root)
