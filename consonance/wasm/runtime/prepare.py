#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
import shutil
from pathlib import Path
import subprocess
import sys
source, output = (Path(value).resolve() for value in sys.argv[1:])
target = output / "wasmi-0.46.0"
if target.exists():
    shutil.rmtree(target)
subprocess.run([sys.executable, str(source.parent / "qualification/prepare-wasmi.py"), str(source / "wasmi-0.46.0.crate"), str(output)], check=True)
subprocess.run(["patch", "--batch", "--fuzz=0", "-p1", "-i", str(source / "numerical.patch")], cwd=target, check=True)
subprocess.run(["patch", "--batch", "--fuzz=0", "-p1", "-i", str(source / "import-completion.patch")], cwd=target, check=True)
subprocess.run(["patch", "--batch", "--fuzz=0", "-p1", "-i", str(source / "validation.patch")], cwd=target, check=True)
lines = (target / "src/lib.rs").read_text().splitlines(keepends=True)
result = []
skipping = False
for line in lines:
    if line.startswith("#!["):
        skipping = not line.rstrip().endswith("]")
        continue
    if skipping:
        if line.rstrip().endswith("]"):
            skipping = False
        continue
    if line.startswith("//!"):
        continue
    result.append(line)
(target / "src/harmony_root.rs").write_text("".join(result))
