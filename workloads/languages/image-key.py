#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Hash the inputs to a language layer, independently of the runtime."""

import hashlib
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[2]
language = sys.argv[1]
if language not in ("c", "rust"):
    raise SystemExit(f"unsupported language: {language}")
paths = list((root / "workloads/languages" / language).rglob("*"))
if language == "c":
    paths.extend(root / "workloads/languages/vendor" / name for name in (
        "antithesis_instrumentation.h", "ANTITHESIS-SDK-CPP-LICENSE",
    ))
    paths.append(root / "workloads/faults/runtime/tests/language_fixture.c")
if language == "rust":
    paths.append(root / "workloads/faults/runtime/tests/language_fixture.rs")
paths.append(root / "workloads/languages/reviewed/bookworm-x86_64.txt")
value = hashlib.sha256()
for path in sorted(paths):
    if path.is_file() and "target" not in path.parts:
        value.update(str(path.relative_to(root)).encode() + b"\0" + path.read_bytes())
print(value.hexdigest())
