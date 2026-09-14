#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Inventory controlled x86 rootfs inputs; verify only explicitly reviewed baselines."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import struct
import subprocess
import tempfile

MAX_FILE = 128 * 1024 * 1024
MAX_TREE = 2 * 1024 * 1024 * 1024
SAVE = {"xsave", "xsave64", "xsavec", "xsavec64", "xsaveopt", "xsaveopt64", "xsaves", "xsaves64"}
RESTORE = {"xrstor", "xrstor64", "xrstors", "xrstors64", "fxsave", "fxsave64", "fxrstor", "fxrstor64"}
SCOPE = {
    "startup_binding": "LD_BIND_NOW=1-before-every-exec",
    "generated_code": "forbidden",
    "writable_executable_memory": "forbidden",
    "workloads": "controlled-trusted",
    "loader_overrides": "forbidden",
}
LIBRARIES = ("/lib/x86_64-linux-gnu", "/usr/lib/x86_64-linux-gnu", "/lib", "/usr/lib")
GLIBC_INTERPRETER = "/lib64/ld-linux-x86-64.so.2"


class Rejected(ValueError):
    pass


def digest(data):
    return hashlib.sha256(data).hexdigest()


def bounded_read(path):
    with path.open("rb") as source:
        data = source.read(MAX_FILE + 1)
    if len(data) > MAX_FILE:
        raise Rejected(f"file exceeds {MAX_FILE} bytes: {path}")
    return data


def guest_path(path):
    if not path.startswith("/") or "\0" in path:
        raise Rejected(f"require absolute guest path: {path!r}")
    parts = []
    for part in path.split("/"):
        if part in ("", "."):
            continue
        if part == "..":
            if not parts:
                raise Rejected(f"guest path escapes root: {path!r}")
            parts.pop()
        else:
            parts.append(part)
    return "/" + "/".join(parts)


def resolve(root, name):
    """Interpret absolute symlinks inside the guest, never against the host root."""
    name = guest_path(name)
    links = []
    for _ in range(41):
        parts = name.strip("/").split("/") if name != "/" else []
        for index in range(len(parts)):
            prefix = "/" + "/".join(parts[:index + 1])
            host = root / prefix.lstrip("/")
            metadata = host.lstat()
            if stat.S_ISLNK(metadata.st_mode):
                target = os.readlink(host)
                links.append({"path": prefix, "target": target})
                base = target if target.startswith("/") else str(Path(prefix).parent / target)
                name = guest_path(base + "/" + "/".join(parts[index + 1:]))
                break
            if index < len(parts) - 1 and not stat.S_ISDIR(metadata.st_mode):
                raise Rejected(f"dependency path traverses a non-directory: {prefix}")
        else:
            return name, links
    raise Rejected(f"symlink cycle/depth at {name}")


def span(data, offset, size):
    if offset < 0 or size < 0 or offset > len(data) or size > len(data) - offset:
        raise Rejected("ELF range outside file")
    return data[offset:offset + size]


def elf(data):
    if len(data) < 64 or data[:7] != b"\x7fELF\x02\x01\x01":
        raise Rejected("require ELF64 little-endian version 1")
    h = struct.unpack_from("<16sHHIQQQIHHHHHH", data)
    if h[1] not in (2, 3) or h[2] != 62 or h[3] != 1 or h[8] != 64:
        raise Rejected("require x86_64 ET_EXEC/ET_DYN ELF")
    if h[9] != 56 or not 0 < h[10] < 65535:
        raise Rejected("unsupported ELF program table")
    span(data, h[5], h[9] * h[10])
    segments, dynamic, interpreter = [], [], None
    seen_dynamic = False
    executable_stack = False
    for index in range(h[10]):
        kind, flags, offset, address, _, filesz, memsz, _ = struct.unpack_from("<IIQQQQQQ", data, h[5] + index * h[9])
        content = span(data, offset, filesz)
        if filesz > memsz or address + memsz > 2**64:
            raise Rejected("invalid ELF segment size/address")
        if kind == 0x6474E551:
            executable_stack = executable_stack or bool(flags & 1)
        if kind == 1:
            segments.append({"offset": offset, "address": address, "size": filesz, "memory_size": memsz, "flags": flags})
        elif kind == 3:
            if interpreter is not None or not content.endswith(b"\0") or b"\0" in content[:-1]:
                raise Rejected("invalid/duplicate ELF interpreter")
            interpreter = content[:-1].decode("utf-8")
            guest_path(interpreter)
        elif kind == 2:
            if seen_dynamic or filesz % 16:
                raise Rejected("invalid/duplicate ELF dynamic segment")
            seen_dynamic = True
            terminated = False
            for entry in range(0, filesz, 16):
                tag, value = struct.unpack_from("<qQ", content, entry)
                if tag == 0:
                    terminated = True
                    break
                dynamic.append((tag, value))
            if not terminated:
                raise Rejected("unterminated ELF dynamic table")
    if not segments:
        raise Rejected("ELF has no load segments")
    for i, first in enumerate(segments):
        for second in segments[:i]:
            if max(first["address"], second["address"]) < min(first["address"] + first["memory_size"], second["address"] + second["memory_size"]):
                raise Rejected("overlapping ELF load segments")
    tags = {}
    for tag, value in dynamic:
        tags.setdefault(tag, []).append(value)
    if any(tag in tags for tag in (0x7FFFFFFD, 0x7FFFFFFF, 0x6FFFFEFB, 0x6FFFFEFC)):
        raise Rejected("unsupported ELF filter/audit dependency")
    strings = b""
    if any(tag in tags for tag in (1, 14, 15, 29)):
        if len(tags.get(5, [])) != 1 or len(tags.get(10, [])) != 1:
            raise Rejected("missing/duplicate ELF dynamic string table")
        address, size = tags[5][0], tags[10][0]
        match = [s for s in segments if s["address"] <= address and address + size <= s["address"] + s["size"]]
        if len(match) != 1:
            raise Rejected("ELF dynamic strings are not file-backed")
        strings = span(data, match[0]["offset"] + address - match[0]["address"], size)

    def string(offset):
        end = strings.find(b"\0", offset)
        if offset >= len(strings) or end < 0:
            raise Rejected("invalid ELF dynamic string")
        return strings[offset:end].decode("utf-8")

    for tag in (14, 15, 29):
        if len(tags.get(tag, [])) > 1:
            raise Rejected("duplicate ELF library search path")
    return {
        "segments": segments, "interpreter": interpreter,
        "executable_stack": executable_stack,
        "dynamic": seen_dynamic,
        "needed": [string(v) for v in tags.get(1, [])],
        "soname": string(tags[14][0]) if 14 in tags else None,
        "rpath": [string(v) for v in tags.get(15, [])],
        "runpath": [string(v) for v in tags.get(29, [])],
        "bind_now": 24 in tags or any(v & 8 for v in tags.get(30, [])) or any(v & 1 for v in tags.get(0x6FFFFFFB, [])),
    }


def disassemble(data, segments, objdump):
    """Disassemble every file-backed executable load segment, including stripped ELFs."""
    instructions = []
    line_re = re.compile(r"^\s*([0-9a-f]+):\s+((?:[0-9a-f]{2}\s+)+)\s*([a-z][a-z0-9]*)\s*(.*)$")
    for segment in segments:
        if not segment["flags"] & 1 or not segment["size"]:
            continue
        with tempfile.NamedTemporaryFile() as binary, tempfile.TemporaryFile(mode="w+") as output:
            binary.write(span(data, segment["offset"], segment["size"]))
            binary.flush()
            result = subprocess.run([objdump, "-D", "-w", "-z", "-b", "binary", "-m", "i386:x86-64", f"--adjust-vma={segment['address']}", binary.name], stdout=output, stderr=subprocess.PIPE, text=True, timeout=60)
            if result.returncode:
                raise Rejected(f"GNU objdump failed: {result.stderr.strip()}")
            output.seek(0)
            count = 0
            for line in output:
                match = line_re.match(line)
                if not match:
                    continue
                count += 1
                if count > 2_000_000:
                    raise Rejected("objdump instruction limit exceeded")
                address, raw, mnemonic, operands = match.groups()
                special = re.search(r"\b(" + "|".join(sorted(SAVE | RESTORE | {"xgetbv"}, key=len, reverse=True)) + r")\b", mnemonic + " " + operands)
                if special:
                    mnemonic = special[1]
                instructions.append({"address": int(address, 16), "bytes": bytes.fromhex(raw).hex(), "mnemonic": mnemonic, "operands": operands.strip()})
            if not count:
                raise Rejected("objdump decoded no instructions")
    targets = set()
    for item in instructions:
        if item["mnemonic"].startswith("j") or item["mnemonic"].startswith("call") or item["mnemonic"].startswith("loop"):
            target = re.match(r"(?:0x)?([0-9a-f]+)(?:\s|$)", item["operands"])
            if target:
                targets.add(int(target[1], 16))
    sites = []
    for index, item in enumerate(instructions):
        if item["mnemonic"] not in SAVE | RESTORE | {"xgetbv"}:
            continue
        item = dict(item)
        if item["mnemonic"] == "xgetbv":
            cursor = item["address"]
            zero = False
            chain = []
            safe = {"mov", "movb", "movw", "movl", "movq", "lea", "leal", "leaq",
                    "and", "andl", "andq", "or", "orl", "orq", "add", "addl", "addq",
                    "sub", "subl", "subq", "cmp", "cmpl", "cmpq", "test", "testl", "testq"}
            for before in reversed(instructions[max(0, index - 16):index]):
                if cursor in targets or before["address"] + len(bytes.fromhex(before["bytes"])) != cursor:
                    break
                operands = re.sub(r"\s", "", before["operands"])
                if (before["mnemonic"] in ("xor", "xorl") and operands == "%ecx,%ecx"
                        or before["mnemonic"] in ("mov", "movl") and operands in ("$0x0,%ecx", "$0,%ecx")):
                    zero = True
                    chain.insert(0, before)
                    break
                if before["mnemonic"] not in safe:
                    break
                destination = operands.rsplit(",", 1)[-1]
                if not before["mnemonic"].startswith(("cmp", "test")) and re.search(r"%(?:rcx|ecx|cx|cl|ch)\b", destination):
                    break
                chain.insert(0, before)
                cursor = before["address"]
            item["adjacent_ecx_zero"] = bool(zero and len(chain) == 1)
            item["straightline_ecx_zero"] = zero
            item["selector_proof_instructions"] = chain if zero else []
            item["requires_reviewed_control_flow_proof"] = True
        sites.append(item)
    return sites


def inventory(root, objdump="objdump"):
    root = root.resolve(strict=True)
    if not root.is_dir():
        raise Rejected("rootfs must be a directory")
    files, artifacts, contents = [], {}, {}
    total = 0
    root_metadata = root.stat()
    try:
        root_xattrs = {key: os.getxattr(root, key, follow_symlinks=False).hex()
                       for key in sorted(os.listxattr(root, follow_symlinks=False))}
    except (OSError, AttributeError, NotImplementedError) as error:
        raise Rejected(f"cannot inventory root extended attributes: {error}") from error
    files.append({"path": "/", "kind": "directory", "mode": stat.S_IMODE(root_metadata.st_mode),
                  "uid": root_metadata.st_uid, "gid": root_metadata.st_gid, "xattrs": root_xattrs})

    def walk(directory):
        nonlocal total
        for entry in sorted(os.scandir(directory), key=lambda e: os.fsencode(e.name)):
            host = Path(entry.path)
            name = "/" + host.relative_to(root).as_posix()
            metadata = entry.stat(follow_symlinks=False)
            item = {"path": name, "mode": stat.S_IMODE(metadata.st_mode),
                    "uid": metadata.st_uid, "gid": metadata.st_gid}
            try:
                item["xattrs"] = {key: os.getxattr(host, key, follow_symlinks=False).hex()
                                  for key in sorted(os.listxattr(host, follow_symlinks=False))}
            except (OSError, AttributeError, NotImplementedError) as error:
                raise Rejected(f"cannot inventory extended attributes: {name}: {error}") from error
            if stat.S_ISLNK(metadata.st_mode):
                item.update(kind="symlink", target=os.readlink(host))
            elif stat.S_ISDIR(metadata.st_mode):
                item["kind"] = "directory"
                files.append(item)
                walk(host)
                continue
            elif stat.S_ISREG(metadata.st_mode):
                data = bounded_read(host)
                total += len(data)
                if total > MAX_TREE:
                    raise Rejected("rootfs byte limit exceeded")
                item.update(kind="file", sha256=digest(data), size=len(data))
                if data.startswith(b"\x7fELF"):
                    record = elf(data)
                    record.update(sha256=digest(data), size=len(data), sites=disassemble(data, record["segments"], objdump))
                    record["writable_executable_segments"] = [s for s in record["segments"] if s["flags"] & 3 == 3]
                    artifacts[name] = record
                    contents[name] = data
            else:
                raise Rejected(f"unsupported rootfs special file: {name}")
            files.append(item)

    walk(root)
    sonames = {}
    for name, record in artifacts.items():
        soname = record["soname"]
        if soname:
            if soname in sonames and sonames[soname] != record["sha256"]:
                raise Rejected(f"ambiguous differing ELF SONAME: {soname}")
            sonames[soname] = record["sha256"]
    for name, record in artifacts.items():
        closure = []
        if record["interpreter"] and record["interpreter"] != GLIBC_INTERPRETER:
            raise Rejected(f"unsupported dynamic interpreter: {name}: {record['interpreter']}")
        if record["needed"] and any(f["path"].startswith("/etc/ld-musl-") for f in files):
            raise Rejected(f"musl loader configuration is not modeled: {name}")
        if record["rpath"] or record["runpath"] or ((record["interpreter"] or record["dynamic"]) and any(f["path"] == "/etc/ld.so.cache" for f in files)):
            raise Rejected(f"unsupported RPATH/RUNPATH or loader cache: {name}")
        if (record["interpreter"] or record["dynamic"]) and any(f["path"].endswith("/glibc-hwcaps") for f in files):
            raise Rejected(f"unsupported hardware-capability library search: {name}")
        wanted = [("interpreter", record["interpreter"])] if record["interpreter"] else []
        wanted += [("needed", n) for n in record["needed"]]
        for kind, dependency in wanted:
            if not dependency:
                raise Rejected(f"empty ELF dependency: {name}")
            if "/" in dependency:
                candidates = [guest_path(dependency)]
            else:
                candidates = [p + "/" + dependency for p in LIBRARIES]
                if dependency == "ld-linux-x86-64.so.2":
                    candidates.insert(0, GLIBC_INTERPRETER)
            matches = []
            for candidate in candidates:
                try:
                    resolved, links = resolve(root, candidate)
                except FileNotFoundError:
                    continue
                if resolved not in artifacts:
                    raise Rejected(f"dependency is not an inventoried x86 ELF: {name} -> {resolved}")
                matches.append({"kind": kind, "requested": dependency, "resolved": resolved, "sha256": artifacts[resolved]["sha256"], "symlinks": links})
            if not matches:
                raise Rejected(f"missing ELF dependency: {name} -> {dependency}")
            if len({match["sha256"] for match in matches}) != 1:
                raise Rejected(f"ambiguous differing ELF dependency: {name} -> {dependency}")
            closure.append(matches[0])
        record["dependencies"] = closure
    for name, record in artifacts.items():
        visited, pending = set(), [edge["resolved"] for edge in record["dependencies"]]
        while pending:
            dependency = pending.pop()
            if dependency in visited:
                continue
            visited.add(dependency)
            pending.extend(edge["resolved"] for edge in artifacts[dependency]["dependencies"])
        record["transitive_dependencies"] = {path: artifacts[path]["sha256"] for path in sorted(visited)}
    return {
        "version": 1, "mode": "candidate", "admitted": False,
        "rootfs_sha256": digest(json.dumps(sorted(files, key=lambda f: f["path"]), sort_keys=True, separators=(",", ":")).encode()),
        "files": files, "artifacts": artifacts,
        "required_scope": SCOPE,
        "limitations": ["Static scan does not enforce startup environment, runtime W^X, or generated-code restrictions.", "Requires immutable controlled rootfs and reviewed trusted control flow; no arbitrary-binary admission.", "Only digest-reviewed glibc at /lib64/ld-linux-x86-64.so.2 is supported; musl loader configuration with dependencies is rejected. Loader overrides are forbidden by reviewed scope; DT_RPATH/DT_RUNPATH, loader caches and glibc-hwcaps with dynamic dependencies are rejected. Default library search ordering requires deployment review."],
    }, contents


def evidence(reference, baseline_dir):
    if not isinstance(reference, dict) or set(reference) != {"file", "sha256"}:
        raise Rejected("proof requires file and sha256")
    relative = Path(reference["file"])
    if relative.is_absolute() or ".." in relative.parts:
        raise Rejected("proof must be relative to baseline directory")
    path = (baseline_dir / relative).resolve(strict=True)
    if not path.is_relative_to(baseline_dir.resolve()):
        raise Rejected("proof escapes baseline directory")
    data = bounded_read(path)
    if not data or digest(data) != reference["sha256"]:
        raise Rejected(f"proof missing/changed: {relative}")


def verify(report, contents, baseline, baseline_dir):
    errors = []
    try:
        if not isinstance(baseline, dict):
            raise Rejected("baseline must be an object")
        if baseline.get("version") != 1 or not isinstance(baseline.get("reviewed_by"), str) or not baseline["reviewed_by"].strip():
            raise Rejected("baseline requires version 1 and a named reviewer")
        if baseline.get("scope") != SCOPE:
            raise Rejected("baseline must record all controlled-scope obligations")
        evidence(baseline.get("scope_evidence"), baseline_dir)
        if baseline.get("rootfs_sha256") != report["rootfs_sha256"]:
            raise Rejected("rootfs digest differs from reviewed baseline")
        approved = baseline.get("artifacts")
        if not isinstance(approved, dict) or set(approved) != set(report["artifacts"]):
            raise Rejected("missing/unreviewed ELF or stale artifact approval")
        dynamic = any(a["interpreter"] or a["needed"] for a in report["artifacts"].values())
        if dynamic:
            loader = baseline.get("glibc_loader", {})
            artifact = report["artifacts"].get(GLIBC_INTERPRETER)
            if not artifact or artifact["soname"] != "ld-linux-x86-64.so.2":
                raise Rejected("missing canonical glibc loader with reviewed SONAME")
            if loader.get("path") != GLIBC_INTERPRETER or loader.get("sha256") != artifact["sha256"]:
                raise Rejected("missing/unreviewed glibc loader digest")
            if loader.get("default_directories") != list(LIBRARIES):
                raise Rejected("unreviewed glibc default directories")
            evidence(loader.get("loader_evidence"), baseline_dir)
        for name, record in report["artifacts"].items():
            item = approved[name]
            if not isinstance(item, dict):
                raise Rejected(f"invalid artifact review: {name}")
            if item.get("sha256") != record["sha256"]:
                raise Rejected(f"ELF digest differs: {name}")
            evidence(item.get("review_evidence"), baseline_dir)
            if record["needed"] or record["interpreter"]:
                if item.get("dependencies") != record["dependencies"] or item.get("transitive_dependencies") != record["transitive_dependencies"]:
                    raise Rejected(f"unreviewed dependency graph: {name}")
            if record["executable_stack"]:
                raise Rejected(f"executable ELF stack: {name}")
            if record["writable_executable_segments"]:
                raise Rejected(f"writable executable ELF segment: {name}")
            selectors = item.get("xgetbv", [])
            if not isinstance(selectors, list) or len({s["address"] for s in selectors}) != len(selectors):
                raise Rejected(f"invalid selector reviews: {name}")
            observed = {s["address"] for s in record["sites"] if s["mnemonic"] == "xgetbv"}
            if {s["address"] for s in selectors} != observed:
                raise Rejected(f"unreviewed/stale XGETBV selector: {name}")
            for site in record["sites"]:
                if site["mnemonic"] == "xgetbv" and not site["straightline_ecx_zero"]:
                    raise Rejected(f"unproved XGETBV selector: {name}:{site['address']:#x}")
            for selector in selectors:
                if selector.get("selector") != 0:
                    raise Rejected(f"XGETBV selector is not zero: {name}")
                evidence(selector.get("control_flow_evidence"), baseline_dir)
            regions = item.get("resolver_regions", [])
            for region in regions:
                if region.get("kind") != "reviewed-eager-resolver" or type(region.get("start")) is not int or type(region.get("size")) is not int or region["size"] <= 0:
                    raise Rejected(f"invalid reviewed resolver region: {name}")
                start, size = region["start"], region["size"]
                segments = [s for s in record["segments"] if s["flags"] & 1 and s["address"] <= start and start + size <= s["address"] + s["size"]]
                if len(segments) != 1:
                    raise Rejected(f"resolver region outside executable bytes: {name}")
                data = span(contents[name], segments[0]["offset"] + start - segments[0]["address"], size)
                if digest(data) != region.get("sha256"):
                    raise Rejected(f"resolver region bytes differ: {name}")
                evidence(region.get("incoming_reference_evidence"), baseline_dir)
                evidence(region.get("eager_binding_evidence"), baseline_dir)
            for site in record["sites"]:
                if site["mnemonic"] in SAVE:
                    covering = [r for r in regions if r["start"] <= site["address"] and site["address"] + len(bytes.fromhex(site["bytes"])) <= r["start"] + r["size"]]
                    if len(covering) != 1:
                        raise Rejected(f"unproved save instruction: {name}:{site['address']:#x} {site['mnemonic']}")
    except (Rejected, OSError, TypeError, KeyError, ValueError, AttributeError) as error:
        errors.append(str(error))
    report.update(mode="verify", admitted=not errors, errors=errors)
    return not errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("inventory", "verify"))
    parser.add_argument("rootfs", type=Path)
    parser.add_argument("--baseline", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--objdump", default="objdump", help="GNU objdump executable")
    args = parser.parse_args()
    try:
        if args.output.resolve().is_relative_to(args.rootfs.resolve()):
            raise Rejected("report must be outside the scanned rootfs")
        report, contents = inventory(args.rootfs, args.objdump)
        success = True
        if args.mode == "verify":
            if args.baseline is None:
                raise Rejected("verify requires an explicitly reviewed --baseline")
            baseline = json.loads(bounded_read(args.baseline))
            success = verify(report, contents, baseline, args.baseline.parent)
    except (Rejected, OSError, ValueError, struct.error, subprocess.SubprocessError) as error:
        report = {"version": 1, "mode": args.mode, "admitted": False, "errors": [str(error)]}
        success = False
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    return 0 if success else 1


if __name__ == "__main__":
    raise SystemExit(main())
