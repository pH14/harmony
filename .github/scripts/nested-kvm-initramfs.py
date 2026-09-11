#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Encode a small newc initramfs, including /dev/console without host mknod."""
import gzip
import os
from pathlib import Path
import stat
import sys

root = Path(sys.argv[1])
archive = bytearray()

def add(name, mode, data=b"", major=0, minor=0):
    encoded = name.encode() + b"\0"
    fields = [1, mode, 0, 0, 1, 0, len(data), 0, 0, major, minor, len(encoded), 0]
    archive.extend(b"070701" + b"".join(f"{n:08x}".encode() for n in fields))
    archive.extend(encoded)
    archive.extend(b"\0" * (-len(archive) % 4))
    archive.extend(data)
    archive.extend(b"\0" * (-len(archive) % 4))

for path in sorted(root.rglob("*")):
    name = path.relative_to(root).as_posix()
    mode = path.lstat().st_mode
    if stat.S_ISDIR(mode):
        add(name, stat.S_IFDIR | 0o755)
    elif stat.S_ISLNK(mode):
        add(name, stat.S_IFLNK | 0o777, os.readlink(path).encode())
    elif stat.S_ISREG(mode):
        add(name, stat.S_IFREG | (mode & 0o777), path.read_bytes())
    else:
        raise ValueError(f"unexpected file type: {name}")
add("dev/console", stat.S_IFCHR | 0o600, major=5, minor=1)
add("TRAILER!!!", 0)
Path(sys.argv[2]).write_bytes(gzip.compress(bytes(archive), mtime=0))
