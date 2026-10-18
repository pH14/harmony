#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Retain and compare bounded native arithmetic observations, including differences."""

import argparse
import hashlib
import itertools
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess


ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "scripts/qualification/native-math/corpus.c"
SCRIPT = Path(__file__).resolve()
FORMAT = 1
EXECUTION_ENVIRONMENT = {}
FIELDS = {"op", "round", "flush", "input", "result", "x87_status", "x87_control", "mxcsr", "errno"}
OPS = {**dict.fromkeys(["libm.sin", "libm.cos", "libm.tan", "libm.exp", "libm.log", "libm.sqrt", "libm.log1p", "libm.expm1"], (16, 16)),
       **dict.fromkeys(["x87.fsin", "x87.fcos", "x87.f2xm1", "x87.fyl2xp1"], (16, 20)),
       **dict.fromkeys(["sse.rcpps", "sse.rsqrtps", "sse.sqrtps", "sse.divps"], (8, 32))}


def digest(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()


def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load(path):
    return json.loads(Path(path).read_bytes(), object_pairs_hook=unique)


def key(record):
    return [record[name] for name in ("op", "round", "flush", "input")]


def records(data):
    result = []
    seen = set()
    for line in data.splitlines():
        record = json.loads(line, object_pairs_hook=unique)
        if not isinstance(record, dict) or set(record) != FIELDS or record["op"] not in OPS:
            raise ValueError("invalid arithmetic record schema")
        for name in ("round", "flush", "x87_status", "x87_control", "mxcsr", "errno"):
            if type(record[name]) is not int:
                raise ValueError("invalid numeric field")
        if record["round"] not in range(4) or record["flush"] not in range(2):
            raise ValueError("invalid floating-point mode")
        for name, width in zip(("input", "result"), OPS[record["op"]]):
            if not isinstance(record[name], str) or not re.fullmatch(f"[0-9a-f]{{{width}}}", record[name]):
                raise ValueError("invalid bit string")
        if not 0 <= record["x87_status"] <= 0xffff or not 0 <= record["mxcsr"] <= 0xffff:
            raise ValueError("invalid floating-point status")
        if record["x87_control"] != 0x037f | (record["round"] << 10):
            raise ValueError("x87 control state changed")
        expected = 0x1f80 | (record["round"] << 13) | (0x8040 if record["flush"] else 0)
        if record["mxcsr"] & ~0x3f != expected:
            raise ValueError("MXCSR control state changed")
        identity = tuple(key(record))
        if identity in seen:
            raise ValueError("duplicate arithmetic case")
        seen.add(identity)
        result.append(record)
    if not result or {row["op"] for row in result} != set(OPS):
        raise ValueError("missing arithmetic operations")
    return result


def execute(command, **kwargs):
    return subprocess.check_output(command, timeout=120, **kwargs)


def bundle(output, cc, revision):
    if platform.system() != "Linux" or platform.machine() != "x86_64":
        raise ValueError("build on Linux x86_64")
    if not re.fullmatch("[0-9a-f]{40}", revision):
        raise ValueError("source revision must be a full Git commit identity")
    output.mkdir(parents=True, exist_ok=False)
    compiler = Path(shutil.which(cc) or cc).resolve(strict=True)
    binary = output / "native-math"
    linkmap = output / "link.map"
    command = [str(compiler), "-std=c11", "-O2", "-Wall", "-Wextra", "-Werror", "-fno-builtin",
               "-fno-fast-math", "-frounding-math", "-ffp-contract=off", "-march=x86-64",
               "-mtune=generic", "-static", str(SOURCE), "-Wl,-Map=" + str(linkmap), "-lm", "-o", str(binary)]
    subprocess.run(command, check=True, timeout=120)
    elf = execute(["readelf", "-l", "-d", str(binary)])
    (output / "elf.txt").write_bytes(elf)
    if b"INTERP" in elf or b"(NEEDED)" in elf:
        raise ValueError("corpus executable is not fully static")
    libraries = {}
    for line in linkmap.read_text().splitlines():
        if line.startswith("LOAD "):
            path = Path(line.removeprefix("LOAD "))
            if path.is_file():
                libraries[str(path)] = digest(path.read_bytes())
    if not any("libm" in Path(path).name for path in libraries):
        raise ValueError("link map did not identify libm")
    tools = {}
    for name in ("as", "ld", "cc1"):
        tool = execute([str(compiler), "-print-prog-name=" + name]).decode().strip()
        tool = Path(shutil.which(tool) or tool).resolve(strict=True)
        tools[name] = {"path": str(tool), "sha256": digest(tool.read_bytes())}
    build_cpu = min(os.sched_getaffinity(0))
    probe = ["taskset", "-c", str(build_cpu), str(binary.resolve())]
    baseline = execute(probe, env=EXECUTION_ENVIRONMENT)
    rows = records(baseline)
    if execute(probe, env=EXECUTION_ENVIRONMENT) != baseline:
        raise ValueError("build-host corpus is not repeatable")
    (output / "build-observations.jsonl").write_bytes(baseline)
    shutil.copyfile(SOURCE, output / "corpus.c")
    manifest = {"format": FORMAT, "scope": "linux-x86_64-native-math-v1", "source_revision": revision,
                "build_cpu": build_cpu,
                "execution_environment": EXECUTION_ENVIRONMENT,
                "qualifier_sha256": digest(SCRIPT.read_bytes()), "binary_sha256": digest(binary.read_bytes()),
                "source_sha256": digest(SOURCE.read_bytes()), "compiler": {"path": str(compiler), "sha256": digest(compiler.read_bytes()),
                "version": execute([str(compiler), "-v"], stderr=subprocess.STDOUT).decode()},
                "command": command, "libraries": libraries, "tools": tools,
                "libc_version": execute(["getconf", "GNU_LIBC_VERSION"]).decode().strip(),
                "link_map_sha256": digest(linkmap.read_bytes()),
                "inventory": [key(row) for row in rows], "build_observations_sha256": digest(baseline)}
    (output / "manifest.json").write_bytes(canonical(manifest))


def verify_bundle(path):
    manifest = load(path / "manifest.json")
    if manifest["format"] != FORMAT or manifest["scope"] != "linux-x86_64-native-math-v1":
        raise ValueError("unsupported corpus identity")
    if manifest["qualifier_sha256"] != digest(SCRIPT.read_bytes()):
        raise ValueError("qualifier differs from retained build")
    if manifest["execution_environment"] != EXECUTION_ENVIRONMENT:
        raise ValueError("corpus execution environment differs")
    for name, field in (("native-math", "binary_sha256"), ("corpus.c", "source_sha256"),
                        ("link.map", "link_map_sha256"), ("build-observations.jsonl", "build_observations_sha256")):
        if digest((path / name).read_bytes()) != manifest[field]:
            raise ValueError(f"bundle artifact changed: {name}")
    rows = records((path / "build-observations.jsonl").read_bytes())
    if [key(row) for row in rows] != manifest["inventory"]:
        raise ValueError("build inventory changed")
    return manifest


def selected_cpu(text, cpu):
    fields = {"vendor_id", "cpu family", "model", "model name", "stepping", "microcode", "flags"}
    for block in text.split("\n\n"):
        values = dict(line.split(":", 1) for line in block.splitlines() if ":" in line)
        values = {name.strip(): value.strip() for name, value in values.items()}
        if values.get("processor") == str(cpu):
            identity = {name: values[name] for name in fields if name in values}
            if set(identity) != fields or identity["vendor_id"] not in {"GenuineIntel", "AuthenticAMD"}:
                raise ValueError("incomplete selected CPU identity")
            identity["flags"] = " ".join(sorted(set(identity["flags"].split())))
            return identity
    raise ValueError("selected CPU missing from /proc/cpuinfo")


def build_identity(manifest):
    return {name: manifest[name] for name in ("source_revision", "source_sha256", "qualifier_sha256",
                                             "compiler", "command", "libraries", "tools", "libc_version")}


def valid_host_id(instance):
    return (isinstance(instance, str) and re.fullmatch(r"[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}", instance)
            and set(instance.replace("-", "")) != {"0"})


def run(path, output, host_id):
    manifest = verify_bundle(path)
    output.mkdir(parents=True, exist_ok=False)
    cpu = min(os.sched_getaffinity(0))
    cpuinfo = Path("/proc/cpuinfo").read_text()
    identity = selected_cpu(cpuinfo, cpu)
    (output / "cpuinfo.txt").write_text(cpuinfo)
    instance = host_id.read_text().strip().lower()
    if not valid_host_id(instance):
        raise ValueError("host ID must be a nonzero firmware UUID")
    binary = path / "native-math"
    binary.chmod(binary.stat().st_mode | 0o100)
    command = ["taskset", "-c", str(cpu), str(binary.resolve())]
    baseline = None
    for repeat in range(3):
        result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=120,
                                env=EXECUTION_ENVIRONMENT)
        (output / f"observations-{repeat}.jsonl").write_bytes(result.stdout)
        (output / f"stderr-{repeat}.txt").write_bytes(result.stderr)
        result.check_returncode()
        rows = records(result.stdout)
        if [key(row) for row in rows] != manifest["inventory"]:
            raise ValueError("execution inventory differs from retained artifact")
        if baseline is not None and result.stdout != baseline:
            raise ValueError("same-host corpus repeats differ; observations retained")
        baseline = result.stdout
    receipt = {"format": FORMAT, "manifest_sha256": digest((path / "manifest.json").read_bytes()),
               "binary_sha256": manifest["binary_sha256"], "host_id": instance, "cpu": cpu,
               "build": build_identity(manifest),
               "execution_environment": EXECUTION_ENVIRONMENT,
               "cpu_identity": identity, "kernel": platform.release(), "repeats": 3,
               "observations_sha256": digest(baseline), "case_count": len(manifest["inventory"])}
    (output / "receipt.json").write_bytes(canonical(receipt))


def compare(path, receipts):
    manifest = verify_bundle(path)
    loaded = []
    for receipt_path in receipts:
        receipt = load(receipt_path)
        if (receipt["format"] != FORMAT or receipt["manifest_sha256"] != digest((path / "manifest.json").read_bytes())
                or receipt["binary_sha256"] != manifest["binary_sha256"] or receipt["repeats"] != 3
                or receipt["build"] != build_identity(manifest)
                or receipt["execution_environment"] != EXECUTION_ENVIRONMENT
                or receipt["case_count"] != len(manifest["inventory"])):
            raise ValueError("receipt does not bind this retained executable and inventory")
        if not valid_host_id(receipt["host_id"]) or type(receipt["cpu"]) is not int or receipt["cpu"] < 0:
            raise ValueError("invalid receipt host identity")
        rows = None
        for repeat in range(3):
            data = (receipt_path.parent / f"observations-{repeat}.jsonl").read_bytes()
            if digest(data) != receipt["observations_sha256"]:
                raise ValueError("receipt observation digest or repeat mismatch")
            rows = records(data)
            if [key(row) for row in rows] != manifest["inventory"]:
                raise ValueError("receipt inventory mismatch")
        actual_cpu = selected_cpu((receipt_path.parent / "cpuinfo.txt").read_text(), receipt["cpu"])
        if actual_cpu != receipt["cpu_identity"]:
            raise ValueError("receipt CPU metadata mismatch")
        loaded.append((receipt, rows))
    hosts = [receipt["host_id"] for receipt, _ in loaded]
    if len(hosts) < 2 or len(set(hosts)) != len(hosts):
        raise ValueError("require at least two distinct firmware host IDs")
    if {receipt["cpu_identity"]["vendor_id"] for receipt, _ in loaded} != {"GenuineIntel", "AuthenticAMD"}:
        raise ValueError("no AMD/Intel pair observed; cross-vendor qualification is incomplete")
    pairs = []
    for (left, a), (right, b) in itertools.combinations(loaded, 2):
        groups = {}
        differences = []
        for x, y in zip(a, b):
            group = f'{x["op"]}/round={x["round"]}/flush={x["flush"]}'
            count = groups.setdefault(group, {"cases": 0, "different": 0})
            count["cases"] += 1
            if x != y:
                count["different"] += 1
                differences.append({"case": key(x), "left": x, "right": y})
        for group in groups.values():
            group["verdict"] = "observed-differences" if group["different"] else "observed-exact"
        pairs.append({"left_host": left["host_id"], "right_host": right["host_id"],
                      "left_cpu": left["cpu_identity"], "right_cpu": right["cpu_identity"],
                      "verdict": "observed-exact" if not differences else "observed-differences",
                      "groups": groups, "differences": differences})
    return {"format": FORMAT, "verdict": "bounded-observations-complete", "scope": manifest["scope"], "binary_sha256": manifest["binary_sha256"],
            "build": build_identity(manifest),
            "manifest_sha256": digest((path / "manifest.json").read_bytes()), "pairs": pairs,
            "claim": "Exact agreement applies only to listed cases, modes, executable and observed CPU pairs. Differences remain unqualified; neither outcome establishes universal arithmetic equivalence."}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    build = sub.add_parser("bundle")
    build.add_argument("output", type=Path)
    build.add_argument("--cc", default="gcc")
    build.add_argument("--source-revision", required=True)
    observe = sub.add_parser("run")
    observe.add_argument("bundle", type=Path)
    observe.add_argument("output", type=Path)
    observe.add_argument("--host-id-file", type=Path, required=True)
    check = sub.add_parser("compare")
    check.add_argument("bundle", type=Path)
    check.add_argument("output", type=Path)
    check.add_argument("receipts", type=Path, nargs="+")
    args = parser.parse_args()
    try:
        if args.command == "bundle":
            bundle(args.output, args.cc, args.source_revision)
        elif args.command == "run":
            run(args.bundle, args.output, args.host_id_file)
        else:
            args.output.write_bytes(canonical(compare(args.bundle, args.receipts)))
    except (ValueError, KeyError, TypeError, OSError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Incomplete native arithmetic evidence: {error}\n")


if __name__ == "__main__":
    main()
