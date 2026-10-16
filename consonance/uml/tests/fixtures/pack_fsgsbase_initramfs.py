#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later

import gzip
import sys
from pathlib import Path


def add_entry(archive: bytearray, inode: int, name: str, mode: int,
              data: bytes = b"", major: int = 0, minor: int = 0) -> None:
    encoded_name = name.encode() + b"\0"
    fields = (inode, mode, 0, 0, 2 if mode & 0o170000 == 0o040000 else 1,
              0, len(data), 0, 0, major, minor, len(encoded_name), 0)
    archive.extend(b"070701")
    archive.extend("".join(f"{field:08x}" for field in fields).encode())
    archive.extend(encoded_name)
    archive.extend(b"\0" * (-len(archive) % 4))
    archive.extend(data)
    archive.extend(b"\0" * (-len(archive) % 4))


def main() -> None:
    fixture = Path(sys.argv[1]).read_bytes()
    output = Path(sys.argv[2])
    archive = bytearray()
    add_entry(archive, 1, "dev", 0o040755)
    add_entry(archive, 2, "dev/console", 0o020600, major=5, minor=1)
    add_entry(archive, 3, "init", 0o100755, fixture)
    add_entry(archive, 4, "TRAILER!!!", 0)
    with output.open("wb") as target, gzip.GzipFile(fileobj=target, mode="wb",
                                                     filename="", mtime=0,
                                                     compresslevel=9) as zipped:
        zipped.write(archive)


if __name__ == "__main__":
    main()
