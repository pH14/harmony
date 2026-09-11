#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Recover bounded raw snapshot evidence from the disposable VM's console."""
import base64
import gzip
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import sys
import tarfile
import tempfile

console = Path(sys.argv[1])
output = Path(sys.argv[2])
if console.stat().st_size > 64 * 1024 * 1024:
    raise ValueError("console exceeds 64 MiB")
lines = console.read_text().splitlines()
start = [i for i, line in enumerate(lines) if line == "SNAPSHOT_EVIDENCE_BEGIN"]
end = [i for i, line in enumerate(lines) if line == "SNAPSHOT_EVIDENCE_END"]
if len(start) != 1 or len(end) != 1 or start[0] >= end[0]:
    raise ValueError("missing, duplicate, or unordered evidence markers")
compressed = base64.b64decode("".join(lines[start[0] + 1 : end[0]]), validate=True)
if len(compressed) > 16 * 1024 * 1024:
    raise ValueError("compressed evidence exceeds 16 MiB")
with gzip.GzipFile(fileobj=io.BytesIO(compressed)) as stream:
    data = stream.read(512 * 1024 * 1024 + 1)
if len(data) > 512 * 1024 * 1024:
    raise ValueError("expanded evidence exceeds 512 MiB")

if output.exists() or output.is_symlink():
    raise ValueError("evidence output already exists")
output.parent.mkdir(parents=True, exist_ok=True)
with tarfile.open(fileobj=io.BytesIO(data), mode="r:") as archive:
    members = []
    names = set()
    logical_size = 0
    for member in archive:
        if len(members) >= 512:
            raise ValueError("too many evidence entries")
        path = PurePosixPath(member.name)
        if path.is_absolute() or ".." in path.parts:
            raise ValueError("unsafe evidence path")
        if path.parts and path.parts[0] == "_transport":
            raise ValueError("reserved evidence path")
        if member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE):
            raise ValueError("unsupported evidence entry type")
        if member.sparse is not None:
            raise ValueError("sparse evidence entries are unsupported")
        if not 0 <= member.size <= 64 * 1024 * 1024:
            raise ValueError("evidence file exceeds 64 MiB")
        logical_size += member.size
        if logical_size > 512 * 1024 * 1024:
            raise ValueError("logical evidence size exceeds 512 MiB")
        name = path.as_posix()
        if name in names:
            raise ValueError("duplicate evidence path")
        names.add(name)
        members.append(member)
    with tempfile.TemporaryDirectory(prefix=".nested-evidence-", dir=output.parent) as temporary:
        ready = Path(temporary) / "ready"
        ready.mkdir()
        hashes = {}
        for member in members:
            destination = ready / member.name
            if member.isdir():
                destination.mkdir(parents=True, exist_ok=True)
                continue
            destination.parent.mkdir(parents=True, exist_ok=True)
            source = archive.extractfile(member)
            if source is None:
                raise ValueError("missing file contents")
            contents = source.read()
            if len(contents) != member.size:
                raise ValueError("truncated evidence file")
            with destination.open("xb") as target:
                target.write(contents)
            hashes[member.name] = hashlib.sha256(contents).hexdigest()
        transport = ready / "_transport"
        transport.mkdir()
        (transport / "evidence.tar.gz").write_bytes(compressed)
        (transport / "files.sha256.json").write_text(json.dumps(hashes, indent=2) + "\n")
        ready.rename(output)
print(f"Recovered {len(hashes)} raw evidence files")
