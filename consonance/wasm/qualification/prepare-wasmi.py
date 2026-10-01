#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Apply the pinned experimental Wasmi extension in disposable storage."""
import argparse
import hashlib
from pathlib import Path, PurePosixPath
import subprocess
import tarfile

VERSION = "0.46.0"

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("archive", type=Path)
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    expected = "c602dd253549139314b217cb18557ac290135ed25a3d806c81d73db6b0effd49"
    if hashlib.sha256(args.archive.read_bytes()).hexdigest() != expected:
        raise SystemExit("Wasmi source checksum mismatch")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    target = output / f"wasmi-{VERSION}"
    if target.exists():
        raise SystemExit("use an empty output directory")
    with tarfile.open(args.archive) as archive:
        members = archive.getmembers()
        for member in members:
            path = PurePosixPath(member.name)
            if path.is_absolute() or ".." in path.parts or path.parts[0] != f"wasmi-{VERSION}" or not (member.isfile() or member.isdir()):
                raise SystemExit("unexpected source archive member")
        archive.extractall(output, members=members)
    patch = Path(__file__).resolve().parent / "wasmi-snapshot.patch"
    subprocess.run(["patch", "--batch", "--fuzz=0", "-p1", "-i", str(patch)], cwd=target, check=True)
    print(target)

if __name__ == "__main__":
    main()
