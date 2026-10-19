#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Exercise exact packaged CLI and runtime bytes through finite public paths."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parent.parent
REGISTRY = ROOT / "scripts/qualification/packaged-host.json"
FIXTURE = ROOT / "cli/tests/fixtures/packaged-host.c"
READBACK = b"HARMONY_PACKAGED_HOST_READBACK=3cde4754b47ec325"
ARCHITECTURES = {"x86_64": ("amd64", 62, "bzImage"), "aarch64": ("arm64", 183, "Image")}
UNAVAILABLE = ("fresh-restore-equivalence", "reused-restore-equivalence", "portable-import-equivalence",
               "whole-state-equivalence", "ordered-history-equivalence", "cross-host-equivalence",
               "hvf-composition", "uml-whole-state")


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def read(path):
    return json.loads(path.read_text())


def write(path, value):
    path.write_text(json.dumps(value, sort_keys=True, indent=2) + "\n")


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def registry():
    record = read(REGISTRY)
    required = {"direct", "same-host-replay", "search-worker", *UNAVAILABLE}
    if (set(record) != {"version", "scope", "required_architectures", "transforms"}
            or record["version"] != 1 or record["required_architectures"] != list(ARCHITECTURES)
            or set(record["transforms"]) != required
            or any(record["transforms"][name] != "unavailable" for name in UNAVAILABLE)):
        raise ValueError("unsupported packaged-host finite matrix")
    return record


def extract(archive, destination, required):
    with tarfile.open(archive, "r:gz") as stream:
        members = stream.getmembers()
        if len(members) != len(required) or {member.name for member in members} != set(required):
            raise ValueError("unexpected or duplicate package members")
        for member in members:
            if not member.isfile() or not 0 < member.size <= 2 * 1024**3:
                raise ValueError("unsupported package member")
            target = destination / required[member.name]
            target.parent.mkdir(parents=True, exist_ok=True)
            with stream.extractfile(member) as source, target.open("xb") as output:
                shutil.copyfileobj(source, output)


def elf(path, architecture):
    with path.open("rb") as stream:
        header = stream.read(64)
    if len(header) != 64 or header[:6] != b"\x7fELF\x02\x01" or int.from_bytes(header[18:20], "little") != ARCHITECTURES[architecture][1]:
        raise ValueError("packaged executable architecture mismatch")


def runtime(root, architecture):
    manifest = read(root / "runtime-manifest.json")
    source = load("runtime_artifacts", ROOT / "consonance/harmony-linux/scripts/runtime-artifacts.py")
    if manifest.get("version") != 1 or manifest.get("architecture") != architecture or manifest.get("source_digest") != source.source_digest(ROOT):
        raise ValueError("packaged runtime is not exact-input for this source")
    for name in (ARCHITECTURES[architecture][2], "initramfs-oci.cpio.gz"):
        if manifest.get("files", {}).get(name) != digest(root / name):
            raise ValueError("packaged runtime file does not match manifest")
    return {"manifest_sha256": digest(root / "runtime-manifest.json"),
            "source_digest": manifest["source_digest"],
            "files": {name: digest(root / name) for name in (ARCHITECTURES[architecture][2], "initramfs-oci.cpio.gz")}}


def trace_workers(directory):
    workers = []
    for path in sorted(directory.glob("search.*")):
        text = path.read_text()
        worker_exec = re.search(r'^(?:execve|execveat)\(.*\[.*"session-worker"\].*\)\s*= 0$', text, re.MULTILINE)
        if not worker_exec:
            continue
        kvm, machines, vcpus, runs = set(), set(), set(), 0
        for line in text[worker_exec.end():].splitlines():
            opened = re.fullmatch(r'open(?:at|at2)?\(.*"/dev/kvm".*\)\s*= ([0-9]+)', line)
            vm = re.fullmatch(r'ioctl\(([0-9]+), KVM_CREATE_VM, .*\)\s*= ([0-9]+)', line)
            vcpu = re.fullmatch(r'ioctl\(([0-9]+), KVM_CREATE_VCPU, .*\)\s*= ([0-9]+)', line)
            run = re.fullmatch(r'ioctl\(([0-9]+), KVM_RUN, .*\)\s*= 0', line)
            closed = re.fullmatch(r'close\(([0-9]+)\)\s*= 0', line)
            if opened:
                kvm.add(int(opened[1]))
            elif vm and int(vm[1]) in kvm:
                machines.add(int(vm[2]))
            elif vcpu and int(vcpu[1]) in machines:
                vcpus.add(int(vcpu[2]))
            elif run and int(run[1]) in vcpus:
                runs += 1
            elif closed:
                for fds in (kvm, machines, vcpus):
                    fds.discard(int(closed[1]))
        if not runs or not text.rstrip().endswith("+++ exited with 0 +++"):
            raise ValueError("worker child lacks successful KVM execution and normal exit")
        workers.append({"pid": int(path.suffix[1:]), "kvm_runs": runs, "trace_sha256": digest(path)})
    if not workers:
        raise ValueError("no successfully executed production worker child")
    return workers


def manifest(directory, artifacts=None):
    record = read(directory / "manifest.json")
    if record.get("status") != "complete" or record.get("error") is not None or not re.fullmatch(r"linux-sealed-static-memfd-elf-sha256-v1:[0-9a-f]{64}", record.get("binary_identity", "")):
        raise ValueError("public execution lacks complete admitted strict-host manifest")
    if record.get("runner_identity", {}).get("backend") != "kvm":
        raise ValueError("public execution did not use KVM")
    if artifacts:
        for name in ("kernel", "initramfs"):
            stored = directory / "artifacts" / name
            if record.get("artifacts", {}).get(name) != digest(stored):
                raise ValueError("public manifest runtime artifact commitment mismatch")
        kernel = artifacts / ARCHITECTURES[platform.machine()][2]
        if digest(directory / "artifacts/kernel") != digest(kernel):
            raise ValueError("public execution used a different kernel")
        base = artifacts / "initramfs-oci.cpio.gz"
        with (directory / "artifacts/initramfs").open("rb") as composed, base.open("rb") as supplied:
            for block in iter(lambda: supplied.read(1024 * 1024), b""):
                if composed.read(len(block)) != block:
                    raise ValueError("public execution used a different base initramfs")
    return record


def command_result(directory):
    record = read(directory / "run.json")
    serial = (directory / "serial.log").read_bytes()
    if record.get("container_rc") != 0 or record.get("supervisor_failure_rc") is not None or record.get("startup_rc") not in (None, 0) or record.get("runtime_rc") not in (None, 0) or record.get("steps", 0) <= 0 or not record.get("terminal"):
        raise ValueError("guest command did not complete successfully")
    if record.get("serial_sha256") != hashlib.sha256(serial).hexdigest() or serial.count(READBACK) != 1:
        raise ValueError("guest readback or serial commitment mismatch")
    return record


def search_result(directory):
    record = read(directory / "report.json")
    if record.get("mode") != "search" or record.get("executions") != 1 or record.get("execution_failures") != 0 or record.get("watchdog_cutoffs") != 0 or record.get("bug_found") is not False:
        raise ValueError("public search did not complete exactly one successful execution")
    summary = read(directory / "campaign-summary.json")
    workers = summary.get("telemetry", {}).get("workers", [])
    if (record.get("execution_ticks", 0) <= 0 or record.get("workers", 0) <= 0
            or len(workers) != record["workers"] or sum(worker.get("jobs", 0) for worker in workers) != 1
            or not any(worker.get("jobs", 0) > 0 and worker.get("target", {}).get("vmm.transport.child.loop.run.receive.completed", 0) > 0 for worker in workers)):
        raise ValueError("public search lacks completed production worker protocol work")
    return {key: record[key] for key in ("executions", "execution_failures", "watchdog_cutoffs", "execution_ticks", "workers")}


def execute(arguments, log, cpus, trace=None):
    command = ["taskset", "-c", cpus]
    if trace:
        command += ["strace", "-ff", "-q", "-s", "256", "-e", "trace=execve,execveat,open,openat,openat2,close,ioctl", "-e", "signal=none", "-o", str(trace / "search")]
    command += list(map(str, arguments))
    with log.open("w") as stream:
        subprocess.run(command, stdout=stream, stderr=subprocess.STDOUT, check=True, timeout=240)


def host():
    return {"architecture": platform.machine(), "kernel": platform.release(), "cpuinfo": Path("/proc/cpuinfo").read_text()}


def qualify(cli_archive, runtime_archive, fixture, output):
    output.mkdir(parents=True, exist_ok=False)
    inputs = {"cli_archive": cli_archive, "runtime_archive": runtime_archive, "fixture": fixture, "fixture_source": FIXTURE, "registry": REGISTRY, "qualifier": Path(__file__)}
    receipt = {"version": 1, "outcome": "failed", "registry_sha256": digest(REGISTRY),
               "qualifier_sha256": digest(Path(__file__)), "qualifier_source": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
               "scope": registry(), "transforms": {name: {"outcome": "unavailable", "reason": "not observed by this bounded public-path cell"} for name in registry()["transforms"]}}
    before = {}
    active = {}
    active_before = {}
    current_transform = None
    try:
        before = {name: digest(path) for name, path in inputs.items()}
        receipt["inputs"] = before
        architecture = platform.machine()
        if platform.system() != "Linux" or architecture not in ARCHITECTURES:
            receipt["outcome"] = "blocked"
            raise ValueError("supported Linux KVM host required")
        receipt["cell"] = f"packaged-host-linux-{architecture}-kvm"
        receipt["host"] = host()
        if not os.access("/dev/kvm", os.R_OK | os.W_OK):
            receipt["outcome"] = "blocked"
            raise ValueError("KVM is unavailable or not writable")
        cpus = sorted(os.sched_getaffinity(0))
        if len(cpus) < 2:
            receipt["outcome"] = "blocked"
            raise ValueError("two admitted CPUs required for coordinator and worker")
        affinity = ",".join(map(str, cpus[:2]))
        binaries, artifacts = output / "bin", output / "runtime"
        extract(cli_archive, binaries, {name: name for name in ("harmony", "harmony-consonance-host")})
        kernel = ARCHITECTURES[architecture][2]
        extract(runtime_archive, artifacts, {f"{architecture}/{name}": name for name in (kernel, "initramfs-oci.cpio.gz", "runtime-manifest.json")})
        for path in (*binaries.iterdir(), fixture):
            elf(path, architecture)
        for path in binaries.iterdir():
            path.chmod(0o755)
        receipt["binaries"] = {path.name: digest(path) for path in binaries.iterdir()}
        receipt["runtime"] = runtime(artifacts, architecture)
        active = {"host:" + path.name: path for path in binaries.iterdir()}
        active.update({"runtime:" + path.name: path for path in artifacts.iterdir()})
        rootfs = output / "rootfs"
        (rootfs / "app").mkdir(parents=True)
        (rootfs / "etc/harmony").mkdir(parents=True)
        (rootfs / "tmp").mkdir()
        shutil.copy2(fixture, rootfs / "app/packaged-host")
        (rootfs / "app/packaged-host").chmod(0o755)
        packager = load("oci_package", ROOT / "workloads/guest-images/oci-package.py")
        image = output / "image"
        receipt["workload"] = {"source_sha256": digest(FIXTURE), "manifest": packager.package(rootfs, ARCHITECTURES[architecture][0], image, ["/app/packaged-host"], ["complete"], [], "0:0", "/")}
        active.update({"workload:" + path.relative_to(image).as_posix(): path for path in image.rglob("*") if path.is_file()})
        active_before = {name: digest(path) for name, path in active.items()}
        receipt["active_files"] = active_before
        config = ('[runner]\nkind="consonance"\nbackend="kvm"\n[runner.options]\nram_mib=512\n' +
                  'kernel=' + json.dumps(str(artifacts / kernel)) + '\nbase_initramfs=' + json.dumps(str(artifacts / "initramfs-oci.cpio.gz")) + '\n')
        cli = binaries / "harmony"
        current_transform = "direct"
        execute([cli, "debug", "run", image, "--config-toml", config, "--for", "60s", "--out", output / "direct"], output / "direct.log", affinity)
        direct = command_result(output / "direct")
        direct_manifest = manifest(output / "direct", artifacts)
        receipt["transforms"]["direct"] = {"outcome": "passed", "record": direct}
        current_transform = "same-host-replay"
        execute([cli, "debug", "replay", output / "direct", "--out", output / "replay"], output / "replay.log", affinity)
        repeated = command_result(output / "replay")
        repeated_manifest = manifest(output / "replay", artifacts)
        if repeated != direct or repeated_manifest["binary_identity"] != direct_manifest["binary_identity"]:
            raise ValueError("same-host public command replay contradiction")
        receipt["transforms"]["same-host-replay"] = {"outcome": "passed", "record": repeated}
        current_transform = None
        traces = output / "trace"
        traces.mkdir()
        scenario = config + '[workload.options.nodes.app]\ncommand=["/app/packaged-host","node"]\n'
        current_transform = "search-worker"
        execute([cli, "search", image, "--config-toml", scenario, "--executions", "1", "--for", "30s", "--out", output / "search"], output / "search.log", affinity, traces)
        search = search_result(output / "search")
        search_manifest = manifest(output / "search", artifacts)
        if search_manifest["binary_identity"] != direct_manifest["binary_identity"]:
            raise ValueError("production search host identity mismatch")
        receipt["transforms"]["search-worker"] = {"outcome": "passed", "workers": trace_workers(traces), "record": search}
        current_transform = None
        receipt["loaded_identity"] = direct_manifest["binary_identity"]
        receipt["outcome"] = "passed"
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        receipt["reason"] = str(error)
        if current_transform:
            receipt["transforms"][current_transform] = {"outcome": "failed", "reason": str(error)}
        raise
    finally:
        def measured(paths):
            result = {}
            for name, path in paths.items():
                try:
                    result[name] = digest(path)
                except OSError:
                    result[name] = None
            return result
        after = measured(inputs)
        active_after = measured(active)
        receipt["active_files_after"] = active_after
        receipt["inputs_after"] = after
        if (before and before != after) or (active_before and active_before != active_after):
            receipt.update(outcome="failed", reason="packaged qualification inputs changed during execution")
            for name, row in receipt["transforms"].items():
                if row["outcome"] == "passed":
                    receipt["transforms"][name] = {"outcome": "failed", "reason": receipt["reason"]}
        write(output / "receipt.json", receipt)
    if receipt["outcome"] != "passed":
        raise ValueError(receipt.get("reason", "incomplete qualification"))
    return receipt


def archive_evidence(directory, output):
    names = ["receipt.json", "direct.log", "replay.log", "search.log"]
    names += [f"{role}/{name}" for role in ("direct", "replay") for name in ("manifest.json", "run.json", "serial.log")]
    names += ["search/" + name for name in ("manifest.json", "report.json", "campaign-summary.json", "stream.jsonl")]
    names += [path.relative_to(directory).as_posix() for path in sorted((directory / "trace").glob("search.*"))]
    names += [path.relative_to(directory).as_posix() for path in sorted((directory / "image").rglob("*")) if path.is_file()]
    with tarfile.open(output, "w:gz") as archive:
        for name in names:
            path = directory / name
            if path.is_symlink():
                raise ValueError("unsupported evidence symlink")
            if path.is_file():
                archive.add(path, arcname=name, recursive=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli-archive", type=Path, required=True)
    parser.add_argument("--runtime-archive", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--evidence-archive", type=Path)
    args = parser.parse_args()
    try:
        qualify(args.cli_archive.resolve(), args.runtime_archive.resolve(), args.fixture.resolve(), args.output.resolve())
    finally:
        if args.evidence_archive:
            archive_evidence(args.output, args.evidence_archive)


if __name__ == "__main__":
    main()
