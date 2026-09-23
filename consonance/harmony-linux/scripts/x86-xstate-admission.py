#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Inventory x86 rootfs inputs and check their executable properties."""

import argparse
import gzip
import io
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
    tlsdesc = any(tag in tags for tag in (0x6ffffef6, 0x6ffffef7))
    tables = [(7, 8, 24), (17, 18, 16)]
    if 23 in tags:
        if tags.get(20) not in ([7], [17]):
            raise Rejected("unsupported PLT relocation format")
        tables.append((23, 2, 24 if tags[20] == [7] else 16))
    for address_tag, size_tag, stride in tables:
        if address_tag not in tags and size_tag not in tags:
            continue
        if len(tags.get(address_tag, [])) != 1 or len(tags.get(size_tag, [])) != 1:
            raise Rejected("missing/duplicate relocation table")
        address, size = tags[address_tag][0], tags[size_tag][0]
        if size % stride:
            raise Rejected("invalid relocation table size")
        if not size:
            continue
        matches = [s for s in segments if s["address"] <= address and address + size <= s["address"] + s["size"]]
        if len(matches) != 1:
            raise Rejected("relocations are not file-backed")
        raw = span(data, matches[0]["offset"] + address - matches[0]["address"], size)
        tlsdesc |= any(struct.unpack_from("<Q", raw, off + 8)[0] & 0xffffffff in (34, 35, 36) for off in range(0, size, stride))
    return {
        "segments": segments, "interpreter": interpreter,
        "executable_stack": executable_stack,
        "dynamic": seen_dynamic,
        "tlsdesc": tlsdesc,
        "text_relocations": 22 in tags or any(v & 4 for v in tags.get(30, [])),
        "needed": [string(v) for v in tags.get(1, [])],
        "soname": string(tags[14][0]) if 14 in tags else None,
        "rpath": [string(v) for v in tags.get(15, [])],
        "runpath": [string(v) for v in tags.get(29, [])],
        "bind_now": 24 in tags or any(v & 8 for v in tags.get(30, [])) or any(v & 1 for v in tags.get(0x6FFFFFFB, [])),
    }


REGISTER_SPILL = r"mov %r(?:[a-d]x|si|di|8|9|10|11),(?:0x[0-9a-f]+)?\(%rsp\)"
REGISTER_RELOAD = r"mov (?:0x[0-9a-f]+)?\(%rsp\),%r(?:[a-d]x|si|di|8|9|10|11)"
TRAMPOLINE_PROLOGUE = [
    (r"mov %rdx,0x[0-9a-f]+\(%rsp\)", 0, 16),
    (r"xor %edx,%edx", 1, 1),
    (r"mov \$(0x[0-9a-f]+),%eax", 1, 1),
    (REGISTER_SPILL, 1, 8),
    (r"sub \S+,%rsp", 1, 1),
    (r"and \$0xffffffffffffffc0,%rsp", 1, 1),
    (r"mov %rsp,%rbx", 1, 1),
]
LOADER_TRAMPOLINES = {
    "eager-resolver": (
        [(r"push %rbx", 1, 1), (r"endbr64", 0, 1)],
        [(r"mov 0x10\(%rbx\),%rsi", 1, 1), (r"mov 0x8\(%rbx\),%rdi", 1, 1),
         (r"call (?:0x)?[0-9a-f]+", 1, 1), (r"mov %rax,%r11", 1, 1),
         (r"mov \$(0x[0-9a-f]+),%eax", 1, 1), (r"xor %edx,%edx", 1, 1),
         (r"xrstor 0x40\(%rsp\)", 1, 1), (REGISTER_RELOAD, 1, 8),
         (r"mov %rbx,%rsp", 1, 1), (r"mov \(%rsp\),%rbx", 1, 1),
         (r"add \$0x18,%rsp", 1, 1), (r"jmp \*%r11", 1, 1)],
    ),
    "unused-tlsdesc": (
        [(r"mov %rbx,-0x18\(%rsp\)", 1, 1)],
        [(r"call (?:0x)?[0-9a-f]+", 1, 1), (r"mov %rax,%rcx", 1, 1),
         (r"mov \$(0x[0-9a-f]+),%eax", 1, 1), (r"xor %edx,%edx", 1, 1),
         (r"xrstor 0x40\(%rsp\)", 1, 1), (r"mov %rcx,%rax", 1, 1),
         (REGISTER_RELOAD, 1, 8), (r"mov %rbx,%rsp", 1, 1),
         (r"mov -0x18\(%rsp\),%rbx", 1, 1), (r"jmp (?:0x)?[0-9a-f]+", 1, 1)],
    ),
}


def instruction_text(item):
    operands = re.sub(r"<[^>]*>", "", item["operands"].split("#", 1)[0])
    text = (item["mnemonic"] + " " + re.sub(r"\s", "", operands)).strip()
    return re.sub(r"^bnd jmp", "jmp ", text)


def match_steps(instructions, start, step, steps):
    index, masks = start, []
    for pattern, low, high in steps:
        count = 0
        while count < high and 0 <= index < len(instructions):
            match = re.fullmatch(pattern, instruction_text(instructions[index]))
            if not match:
                break
            masks.extend(match.groups())
            index += step
            count += 1
        if count < low:
            return None
    return index - step, masks


def loader_trampoline(instructions, index, targets):
    """Name the glibc loader trampoline holding a save, or None.

    The lazy-binding resolver never runs under LD_BIND_NOW=1, and the dynamic
    TLS descriptor trampoline never runs without TLSdesc relocations. Both are
    matched by their complete straight-line shape, so no address is pinned."""
    if instruction_text(instructions[index]).split(" ", 1)[-1] != "0x40(%rsp)":
        return None
    prologue = match_steps(instructions, index - 1, -1, TRAMPOLINE_PROLOGUE)
    if prologue is None:
        return None
    for kind, (entry_steps, body_steps) in LOADER_TRAMPOLINES.items():
        entry = match_steps(instructions, prologue[0] - 1, -1, entry_steps)
        body = match_steps(instructions, index + 1, 1, body_steps)
        if entry is None or body is None or set(prologue[1]) != set(body[1]) or len(set(body[1])) != 1:
            continue
        first, last = entry[0], body[0]
        span_items = instructions[first:last + 1]
        if any(a["address"] + len(bytes.fromhex(a["bytes"])) != b["address"] for a, b in zip(span_items, span_items[1:])):
            continue
        if any(item["address"] in targets for item in span_items[1:]):
            continue
        return kind
    return None


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
        target = re.search(r"\b(?:j[a-z]*|call[a-z]*|loop[a-z]*)\s+(?:0x)?([0-9a-f]+)(?:\s|$)", item["mnemonic"] + " " + item["operands"])
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
        if item["mnemonic"] in SAVE:
            item["loader_trampoline"] = loader_trampoline(instructions, index, targets)
        sites.append(item)
    return sites


def inventory(root, objdump="objdump", archive_manifest=None):
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
    if archive_manifest is not None:
        files = archive_manifest
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


def parse_newc(data):
    """Parse one newc archive; device entries stay inert metadata."""
    position, entries, names = 0, [], set()
    while True:
        header = span(data, position, 110)
        if header[:6] != b"070701":
            raise Rejected("require uncompressed newc (070701) records")
        if not re.fullmatch(b"[0-9a-fA-F]{104}", header[6:]):
            raise Rejected("invalid newc hexadecimal header")
        fields = [int(header[6 + i * 8:14 + i * 8], 16) for i in range(13)]
        ino, mode, uid, gid, nlink, mtime, size, devmajor, devminor, rmajor, rminor, namesize, checksum = fields
        if checksum or not 1 <= namesize <= 4096 or size > MAX_FILE:
            raise Rejected("unsupported newc checksum/name/content size")
        position += 110
        rawname = span(data, position, namesize)
        if not rawname.endswith(b"\0") or b"\0" in rawname[:-1]:
            raise Rejected("invalid newc name")
        name = rawname[:-1].decode("utf-8")
        position += namesize
        padding = (-position) % 4
        if any(span(data, position, padding)):
            raise Rejected("nonzero newc name padding")
        position += padding
        content = span(data, position, size)
        position += size
        padding = (-position) % 4
        if any(span(data, position, padding)):
            raise Rejected("nonzero newc content padding")
        position += padding
        if name == "TRAILER!!!":
            if size or any(data[position:]):
                raise Rejected("newc trailer content or concatenated/trailing archive")
            return entries
        if not name or name.startswith("/") or ".." in name.split("/"):
            raise Rejected("absolute/traversing newc path")
        normalized = guest_path("/" + name)
        if normalized in names:
            raise Rejected(f"duplicate newc path: {normalized}")
        names.add(normalized)
        if len(names) > 100000:
            raise Rejected("newc entry count exceeded")
        kind = stat.S_IFMT(mode)
        kinds = {stat.S_IFREG: "file", stat.S_IFDIR: "directory", stat.S_IFLNK: "symlink",
                 stat.S_IFCHR: "char-device", stat.S_IFBLK: "block-device"}
        if kind not in kinds or mode & ~0xFFFF or nlink == 0:
            raise Rejected(f"unsupported newc entry type/mode/link count: {normalized}")
        if kind != stat.S_IFDIR and nlink != 1:
            raise Rejected(f"newc hardlinks are unsupported: {normalized}")
        if kind not in (stat.S_IFREG, stat.S_IFLNK) and size:
            raise Rejected(f"newc metadata entry has content: {normalized}")
        if kind not in (stat.S_IFCHR, stat.S_IFBLK) and (rmajor or rminor):
            raise Rejected(f"newc non-device has device number: {normalized}")
        if normalized == "/" and kind != stat.S_IFDIR:
            raise Rejected("newc root must be a directory")
        item = {"path": normalized, "kind": kinds[kind], "mode": stat.S_IMODE(mode),
                "uid": uid, "gid": gid, "inode": ino, "nlink": nlink, "mtime": mtime,
                "archive_device": [devmajor, devminor], "rdev": [rmajor, rminor], "xattrs": {}}
        if kind == stat.S_IFREG:
            item.update(size=size, sha256=digest(content))
        elif kind == stat.S_IFLNK:
            target = (content[:-1] if content.endswith(b"\0") else content).decode("utf-8")
            if not target or "\0" in target:
                raise Rejected("invalid newc symlink target")
            item.update(target=target, size=size, sha256=digest(content))
        entries.append((item, content))


def inventory_initramfs(path, objdump="objdump"):
    compressed = bounded_read(path)
    if compressed.startswith(b"\x1f\x8b"):
        with gzip.GzipFile(fileobj=io.BytesIO(compressed)) as stream:
            data = stream.read(MAX_TREE + 1)
    else:
        data = compressed
    if len(data) > MAX_TREE:
        raise Rejected("uncompressed initramfs byte limit exceeded")
    entries = parse_newc(data)
    manifest = sorted([item for item, _ in entries], key=lambda item: item["path"])
    types = {item["path"]: item["kind"] for item in manifest}
    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        for item in manifest:
            parent = Path(item["path"]).parent
            while str(parent) != "/":
                if types.get(str(parent)) != "directory":
                    raise Rejected(f"newc parent absent or not a directory: {item['path']}")
                parent = parent.parent
        for item in manifest:
            if item["kind"] == "directory":
                (root / item["path"].lstrip("/")).mkdir(parents=True, exist_ok=True)
        for item, content in entries:
            host = root / item["path"].lstrip("/")
            if item["kind"] == "file":
                host.write_bytes(content)
            elif item["kind"] == "symlink":
                host.symlink_to(item["target"])
        report, contents = inventory(root, objdump, archive_manifest=manifest)
    report["files"] = manifest
    report["rootfs_sha256"] = digest(json.dumps(manifest, sort_keys=True, separators=(",", ":")).encode())
    report["archive_sha256"] = digest(compressed)
    report["archive_format"] = "gzip-newc" if compressed.startswith(b"\x1f\x8b") else "newc"
    report["limitations"].append("CPIO device entries are metadata only; no host device nodes are created. Hardlinks, CRC records, concatenated archives and special types other than char/block devices are unsupported.")
    return report, contents


def verify(report):
    errors = []
    try:
        if any(a["interpreter"] or a["needed"] for a in report["artifacts"].values()):
            loader = report["artifacts"].get(GLIBC_INTERPRETER)
            if not loader or loader["soname"] != "ld-linux-x86-64.so.2":
                raise Rejected("missing canonical glibc loader")
        if any(record["tlsdesc"] for record in report["artifacts"].values()):
            raise Rejected("TLSdesc relocation in executable closure")
        for name, record in report["artifacts"].items():
            if record["text_relocations"]:
                raise Rejected(f"ELF text relocations: {name}")
            if record["executable_stack"]:
                raise Rejected(f"executable ELF stack: {name}")
            if record["writable_executable_segments"]:
                raise Rejected(f"writable executable ELF segment: {name}")
            if any(s["mnemonic"] == "xgetbv" and not s["straightline_ecx_zero"] for s in record["sites"]):
                raise Rejected(f"unproved XGETBV selector: {name}")
            for site in record["sites"]:
                if site["mnemonic"] in SAVE and not site.get("loader_trampoline"):
                    raise Rejected(f"save instruction outside a loader trampoline: {name}:{site['address']:#x} {site['mnemonic']}")
    except (Rejected, OSError, TypeError, KeyError, ValueError, AttributeError) as error:
        errors.append(str(error))
    report.update(mode="verify", admitted=not errors, errors=errors)
    return not errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("inventory", "verify", "inventory-initramfs", "verify-initramfs"))
    parser.add_argument("rootfs", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--objdump", default="objdump", help="GNU objdump executable")
    args = parser.parse_args()
    try:
        if args.output.resolve().is_relative_to(args.rootfs.resolve()):
            raise Rejected("report must be outside the scanned rootfs")
        inspect = inventory_initramfs if args.mode.endswith("-initramfs") else inventory
        report, _ = inspect(args.rootfs, args.objdump)
        success = True
        if args.mode.startswith("verify"):
            success = verify(report)
    except (Rejected, OSError, ValueError, EOFError, struct.error, subprocess.SubprocessError) as error:
        report = {"version": 1, "mode": args.mode, "admitted": False, "errors": [str(error)]}
        success = False
    args.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    return 0 if success else 1


if __name__ == "__main__":
    raise SystemExit(main())
