#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Retain one Linux continuation artifact, export a cut, and qualify its imports."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys

SPEC = importlib.util.spec_from_file_location("qualification", Path(__file__).with_name("qualify-consonance.py"))
Q = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(Q)
ROOT = Q.ROOT
REGISTRY = ROOT / "scripts/qualification/linux-continuation.json"
EXECUTABLE = "linux_snapshot_continuation"
TEST = "composed_linux_snapshot_continuation"
CUT_FILES = ("memory.bin", "state.bin", "observation.txt", "host-class.txt")
FILES = (EXECUTABLE, "bzImage", "initramfs-continuation.cpio.gz")


def cell():
    value = Q.read(REGISTRY)
    Q.fields(value, "version cell")
    result = value["cell"]
    Q.fields(result, "id host_class backend test scope oracles unsupported minimum_import_hosts")
    if value["version"] != 1 or result["test"] != TEST or result["minimum_import_hosts"] < 2 or len(result["oracles"]) != 9:
        raise ValueError("unsupported or vacuous Linux continuation cell")
    return result


def source():
    return subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()


def hashes(directory, names):
    return {name: Q.digest(directory / name) for name in names}


def identity(directory):
    value = Q.read(directory / "identity.json")
    Q.fields(value, "version source files registry_sha256 contract_sha256")
    if value != {"version": 1, "source": source(), "files": hashes(directory, FILES),
                 "registry_sha256": Q.digest(REGISTRY),
                 "contract_sha256": Q.digest(ROOT / "consonance/vmm-core/contracts/x86/guest.toml")}:
        raise ValueError("Linux continuation artifact identity mismatch")
    return value


def bundle(directory, kernel, initramfs):
    cell()
    if subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=no"], cwd=ROOT):
        raise ValueError("commit source before retaining an executable")
    result = subprocess.run(["cargo", "test", "--locked", "--release", "-p", "vmm-core", "--test", EXECUTABLE,
                             "--target", "x86_64-unknown-linux-musl", "--no-run", "--message-format=json"],
                            cwd=ROOT, text=True, stdout=subprocess.PIPE, check=True)
    binaries = [record["executable"] for line in result.stdout.splitlines()
                if (record := json.loads(line)).get("reason") == "compiler-artifact"
                and record.get("executable") and record["target"]["name"] == EXECUTABLE]
    if len(binaries) != 1:
        raise ValueError("expected one retained Linux continuation executable")
    directory.mkdir(parents=True, exist_ok=False)
    for original, name in zip((Path(binaries[0]), kernel, initramfs), FILES):
        shutil.copy2(original, directory / name)
    Q.write(directory / "identity.json", {"version": 1, "source": source(), "files": hashes(directory, FILES),
            "registry_sha256": Q.digest(REGISTRY),
            "contract_sha256": Q.digest(ROOT / "consonance/vmm-core/contracts/x86/guest.toml")})


def marker(log, key, pattern):
    matches = re.findall(rf"(?:^|\.\.\. )X3_{key}=({pattern})$", log, re.MULTILINE)
    if len(matches) != 1:
        raise ValueError(f"missing or duplicate X3 {key}")
    return matches[0]


def evidence(log, kind):
    if not re.search(r"test result: ok\. 1 passed; 0 failed; 0 ignored;", log):
        raise ValueError("incomplete or failed Linux continuation execution")
    if kind == "import":
        status = marker(log, "IMPORT", "matched|refused-host-core-class-unchanged")
        classes = {key: marker(log, key.upper(), r"None|Some\(Performance\)|Some\(Efficiency\)") for key in ("source_class", "target_class")}
        if (classes["source_class"] == classes["target_class"]) != (status == "matched"):
            raise ValueError("import admission outcome does not match the retained host core classes")
        return {"import": status, **classes, "observation": marker(log, "OBSERVATION", r"X3_RESULT(?: [0-9a-f]{16}){7} ?") if status == "matched" else None}
    return {
        "observation": marker(log, "OBSERVATION", r"X3_RESULT(?: [0-9a-f]{16}){7} ?"),
        "reused": marker(log, "REUSED", "matched|refused-amd-unchanged-usable"),
        "negative": marker(log, "RAM_NEGATIVE", "detected"),
        "cpu_negative": marker(log, "CPU_NEGATIVE", "detected"),
        "local": marker(log, "LOCAL", "uninterrupted,capture-neutral,fresh,pressure,reused-or-refused"),
    }


def checked_receipt(path, artifact, kind):
    record = Q.read(path)
    Q.fields(record, "version cell artifact kind host log_sha256 evidence cut")
    Q.fields(record["host"], "instance_id hostname kernel vendor cpuinfo")
    Q.host_instance_id(record["host"]["instance_id"])
    if record["host"]["vendor"] not in ("GenuineIntel", "AuthenticAMD"):
        raise ValueError("unknown x86 host vendor")
    if set(re.findall(r"^vendor_id\s*:\s*(\S+)", record["host"]["cpuinfo"], re.MULTILINE)) != {record["host"]["vendor"]}:
        raise ValueError("receipt vendor does not match retained CPU inventory")
    if record["version"] != 1 or record["cell"] != cell() or record["artifact"] != artifact or record["kind"] != kind:
        raise ValueError("mismatched Linux continuation receipt")
    log = path.parent / "execution.log"
    if Q.digest(log) != record["log_sha256"] or evidence(log.read_text(), kind) != record["evidence"]:
        raise ValueError("receipt does not match retained execution log")
    Q.fields(record["cut"], "memory.bin state.bin observation.txt host-class.txt")
    if any(not isinstance(value, str) or not re.fullmatch("[0-9a-f]{64}", value) for value in record["cut"].values()):
        raise ValueError("invalid snapshot identities")
    if kind == "local":
        expected_reused = "refused-amd-unchanged-usable" if record["host"]["vendor"] == "AuthenticAMD" else "matched"
        if record["evidence"]["reused"] != expected_reused:
            raise ValueError("reused restore outcome does not match the host vendor contract")
        if hashes(path.parent / "cut", CUT_FILES) != record["cut"]:
            raise ValueError("snapshot identity mismatch")
        if (path.parent / "cut/observation.txt").read_text() != record["evidence"]["observation"]:
            raise ValueError("snapshot expected observation mismatch")
    return record


def run(directory, output, host_id_file, exported=None):
    artifact = identity(directory)
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise ValueError("unrun: Linux x86-64 KVM required")
    cpuinfo = Path("/proc/cpuinfo").read_text()
    vendors = set(re.findall(r"^vendor_id\s*:\s*(\S+)", cpuinfo, re.MULTILINE))
    if len(vendors) != 1:
        raise ValueError("missing or inconsistent host vendor")
    kind = "import" if exported else "local"
    prior = checked_receipt(exported / "receipt.json", artifact, "local") if exported else None
    output.mkdir(parents=True, exist_ok=False)
    executable = directory / EXECUTABLE
    executable.chmod(executable.stat().st_mode | 0o111)
    environment = {key: value for key, value in os.environ.items() if not key.startswith("X3_")}
    environment["X3_ARTIFACTS"] = str(directory.resolve())
    environment["X3_IMPORT" if exported else "X3_EXPORT"] = str((exported / "cut" if exported else output / "cut").resolve())
    with (output / "execution.log").open("w") as log:
        subprocess.run([str(executable.resolve()), TEST, "--ignored", "--exact", "--nocapture", "--test-threads=1"],
                       env=environment, stdout=log, stderr=subprocess.STDOUT, timeout=1800, check=True)
    log = output / "execution.log"
    Q.write(output / "receipt.json", {"version": 1, "cell": cell(), "artifact": artifact, "kind": kind,
        "host": {"instance_id": Q.host_instance_id(host_id_file.read_text().strip()), "hostname": platform.node(),
                 "kernel": platform.release(), "vendor": vendors.pop(), "cpuinfo": cpuinfo},
        "log_sha256": Q.digest(log), "evidence": evidence(log.read_text(), kind),
        "cut": prior["cut"] if prior else hashes(output / "cut", CUT_FILES)})


def compare(directory, exported, receipts, output):
    verdict = {"version": 1, "cell": None, "outcome": "inconclusive", "imports": []}
    try:
        verdict["cell"] = cell()
        artifact = identity(directory)
        original = checked_receipt(exported, artifact, "local")
        source_class = (exported.parent / "cut/host-class.txt").read_text()
        seen = {original["host"]["instance_id"]}
        cross_vendor = False
        for path in receipts:
            imported = checked_receipt(path, artifact, "import")
            host = imported["host"]
            if host["instance_id"] in seen or imported["cut"] != original["cut"]:
                raise ValueError("duplicate host or substituted snapshot")
            seen.add(host["instance_id"])
            status = imported["evidence"]["import"]
            if imported["evidence"]["source_class"] != source_class:
                raise ValueError("import source class differs from retained snapshot source")
            if status == "matched" and imported["evidence"]["observation"] != original["evidence"]["observation"]:
                verdict["outcome"] = "diverged"
                raise ValueError("cross-host guest observation contradiction")
            cross_vendor |= host["vendor"] != original["host"]["vendor"] and status == "matched"
            verdict["imports"].append({"host": host, "outcome": status})
        if len(receipts) < cell()["minimum_import_hosts"] or not cross_vendor:
            raise ValueError("missing supported cross-vendor import or required distinct hosts")
        verdict.update(outcome="matched", artifact=artifact, exporter=original["host"], cut=original["cut"])
    except (ValueError, OSError) as error:
        verdict["reason"] = str(error)
        raise
    finally:
        Q.write(output, verdict)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("bundle")
    build.add_argument("directory", type=Path)
    build.add_argument("--kernel", type=Path, required=True)
    build.add_argument("--initramfs", type=Path, required=True)
    execute = commands.add_parser("run")
    execute.add_argument("directory", type=Path)
    execute.add_argument("output", type=Path)
    execute.add_argument("--host-id-file", type=Path, required=True)
    execute.add_argument("--import-from", type=Path)
    execute.add_argument("--ignored", action="store_true", required=True)
    execute.add_argument("--test", choices=[f"{EXECUTABLE}::{TEST}"], required=True)
    check = commands.add_parser("compare")
    check.add_argument("directory", type=Path)
    check.add_argument("exported", type=Path)
    check.add_argument("output", type=Path)
    check.add_argument("receipts", type=Path, nargs="+")
    args = parser.parse_args()
    try:
        if args.command == "bundle":
            bundle(args.directory, args.kernel, args.initramfs)
        elif args.command == "run":
            run(args.directory, args.output, args.host_id_file, args.import_from)
        else:
            compare(args.directory, args.exported, args.receipts, args.output)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        print(f"Linux continuation qualification refused: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
