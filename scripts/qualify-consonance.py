#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run retained production test artifacts and check registered qualification cells."""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
REGISTRY = ROOT / "scripts/qualification/consonance.json"
TESTS = (
    "x1_ten_same_seed_runs_produce_one_normalized_log",
    "x1_comparator_catches_one_exit_late_delivery_on_this_workload",
    "x1_placement_checker_catches_consistently_late_twins",
)


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def read(path):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate field {key}")
            result[key] = value
        return result
    return json.loads(path.read_text(), object_pairs_hook=unique)


def fields(value, expected):
    if not isinstance(value, dict) or set(value) != set(expected.split()):
        raise ValueError(f"expected fields: {expected}")


def registry():
    value = read(REGISTRY)
    fields(value, "version cells")
    if value["version"] != 1 or not value["cells"]:
        raise ValueError("empty or unsupported qualification registry")
    ids = set()
    for cell in value["cells"]:
        fields(cell, "id host_class backend relation state_scope restore_mode instruction_surface oracles replicas tests")
        if cell["id"] in ids or not cell["oracles"] or cell["replicas"] < 2:
            raise ValueError("duplicate, empty or vacuous qualification cell")
        if cell["tests"] != list(TESTS) or cell["id"] != "x1-kvm-cross-host":
            raise ValueError("no production adapter for registered cell")
        ids.add(cell["id"])
    return value


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def bundle(output):
    registry()
    result = subprocess.run([
        "cargo", "test", "--locked", "-p", "vmm-core", "--test", "x86_kvm_virtual_time",
        "--target", "x86_64-unknown-linux-musl", "--no-run", "--message-format=json",
    ], cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True)
    binaries = []
    for line in result.stdout.splitlines():
        record = json.loads(line)
        if record.get("reason") == "compiler-artifact" and record.get("executable") and record["target"]["name"] == "x86_kvm_virtual_time":
            binaries.append(Path(record["executable"]))
    if len(binaries) != 1:
        raise ValueError("expected exactly one production test executable")
    output.mkdir(parents=True, exist_ok=True)
    shutil.copy2(binaries[0], output / "x86_kvm_virtual_time")
    write(output / "identity.json", {
        "version": 1,
        "source": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
        "executable_sha256": digest(output / "x86_kvm_virtual_time"),
        "registry_sha256": digest(REGISTRY),
        "guest": "embedded-x1-real-mode-guest-and-interrupt-handler",
        "contract_sha256": digest(ROOT / "consonance/vmm-core/contracts/x86/guest.toml"),
    })


def identity(bundle_dir):
    value = read(bundle_dir / "identity.json")
    fields(value, "version source executable_sha256 registry_sha256 guest contract_sha256")
    if value["version"] != 1 or value["executable_sha256"] != digest(bundle_dir / "x86_kvm_virtual_time"):
        raise ValueError("retained executable identity mismatch")
    if value["registry_sha256"] != digest(REGISTRY):
        raise ValueError("registry identity mismatch")
    if value["contract_sha256"] != digest(ROOT / "consonance/vmm-core/contracts/x86/guest.toml"):
        raise ValueError("contract identity mismatch")
    if value["source"] != subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip():
        raise ValueError("source identity mismatch")
    return value


def parse_evidence(log):
    evidence = {}
    for key, pattern in (("digest", "[0-9a-f]{64}"), ("events", "[0-9]+"), ("guest_deliveries", "[0-9]+"), ("checkpoints", "[0-9]+")):
        matches = re.findall(rf"^X1_{key.upper()}=({pattern})$", log, re.MULTILINE)
        if len(matches) != 1:
            raise ValueError(f"missing or duplicate X1 {key}")
        evidence[key] = matches[0] if key == "digest" else int(matches[0])
    if evidence["events"] != 6 or evidence["guest_deliveries"] != 2 or evidence["checkpoints"] != 3:
        raise ValueError("incomplete guest execution")
    return evidence


def run(bundle_dir, output, replica):
    cells = registry()["cells"]
    artifact = identity(bundle_dir)
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise ValueError("unrun: adapter requires Linux x86-64 KVM")
    output.mkdir(parents=True, exist_ok=True)
    executable = bundle_dir / "x86_kvm_virtual_time"
    executable.chmod(0o755)
    results = {}
    for test in TESTS:
        result = subprocess.run([str(executable.resolve()), test, "--ignored", "--exact", "--test-threads=1", "--nocapture"], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=180)
        (output / f"{test}.log").write_text(result.stdout)
        if result.returncode or "test result: ok. 1 passed; 0 failed; 0 ignored;" not in result.stdout:
            raise ValueError(f"failed or unrun: {test}")
        results[test] = digest(output / f"{test}.log")
    log = (output / f"{TESTS[0]}.log").read_text()
    evidence = parse_evidence(log)
    write(output / "receipt.json", {
        "version": 1, "cell": cells[0], "artifact": artifact, "replica": replica,
        "host": {"hostname": platform.node(), "kernel": platform.release(), "cpuinfo": Path("/proc/cpuinfo").read_text()},
        "tests": results, "evidence": evidence,
    })


def compare(bundle_dir, receipts, output):
    verdict = {"version": 1, "cells": [], "outcome": "inconclusive"}
    try:
        manifest = registry()
        artifact = identity(bundle_dir)
        verdict["artifact"] = artifact
        records = [read(path) for path in receipts]
        for record in records:
            fields(record, "version cell artifact replica host tests evidence")
            if record["version"] != 1 or record["artifact"] != artifact or record["cell"] not in manifest["cells"]:
                raise ValueError("unregistered cell or mismatched retained artifacts")
            fields(record["host"], "hostname kernel cpuinfo")
            if any(not isinstance(value, str) or not value for value in record["host"].values()):
                raise ValueError("missing host identity")
            fields(record["evidence"], "digest events guest_deliveries checkpoints")
            if not re.fullmatch("[0-9a-f]{64}", str(record["evidence"]["digest"])) or record["evidence"]["events"] != 6 or record["evidence"]["guest_deliveries"] != 2 or record["evidence"]["checkpoints"] != 3:
                raise ValueError("vacuous or malformed checkpoint evidence")
            if set(record["tests"]) != set(TESTS):
                raise ValueError("missing oracle or negative control")
        for path, record in zip(receipts, records):
            for test, expected in record["tests"].items():
                log = path.parent / f"{test}.log"
                if digest(log) != expected or "test result: ok. 1 passed; 0 failed; 0 ignored;" not in log.read_text():
                    raise ValueError("missing or corrupted oracle evidence")
            if parse_evidence((path.parent / f"{TESTS[0]}.log").read_text()) != record["evidence"]:
                raise ValueError("receipt does not match execution log")
        for cell in manifest["cells"]:
            matches = [record for record in records if record["cell"] == cell]
            if len(matches) != cell["replicas"] or len({record["replica"] for record in matches}) != cell["replicas"] or len({record["host"]["hostname"] for record in matches}) != cell["replicas"]:
                raise ValueError(f"missing or duplicate required replicas for {cell['id']}")
            if any(record["evidence"] != matches[0]["evidence"] for record in matches):
                verdict["outcome"] = "diverged"
                raise ValueError(f"cross-host contradiction for {cell['id']}")
            verdict["cells"].append({"cell": cell, "hosts": [record["host"] for record in matches], "evidence": matches[0]["evidence"], "outcome": "matched"})
        verdict["outcome"] = "matched"
    except (ValueError, OSError) as error:
        verdict["reason"] = str(error)
        raise
    finally:
        write(output, verdict)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("bundle")
    build.add_argument("output", type=Path)
    execute = commands.add_parser("run")
    execute.add_argument("executable", type=Path)
    execute.add_argument("output", type=Path)
    execute.add_argument("--replica", required=True)
    execute.add_argument("--ignored", action="store_true", required=True)
    check = commands.add_parser("compare")
    check.add_argument("bundle", type=Path)
    check.add_argument("output", type=Path)
    check.add_argument("receipts", nargs="+", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "bundle":
            bundle(args.output)
        elif args.command == "run":
            if args.executable.name != "x86_kvm_virtual_time":
                raise ValueError("unregistered production executable")
            run(args.executable.parent, args.output, args.replica)
        else:
            compare(args.bundle, args.receipts, args.output)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"qualification refused: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
