#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Hash the inputs to a language layer, independently of the runtime."""

import hashlib
import sys
from pathlib import Path

root = Path(__file__).resolve().parents[2]
language = sys.argv[1]
if language not in ("c", "rust", "go"):
    raise SystemExit(f"unsupported language: {language}")
paths = list((root / "workloads/languages" / language).rglob("*"))
if language == "c":
    paths.extend(root / "workloads/languages/vendor" / name for name in (
        "antithesis_instrumentation.h", "ANTITHESIS-SDK-CPP-LICENSE",
    ))
    paths.append(root / "workloads/faults/runtime/tests/language_fixture.c")
if language == "rust":
    paths.append(root / "workloads/faults/runtime/tests/language_fixture.rs")
if language == "go":
    paths = [root / "workloads/languages/go" / name for name in (
        "Dockerfile", "configure-stdlib.sh", "antithesis-go-toolexec-v0.8.0-stdlib.patch",
    )]
    paths.extend((root / "consonance/harmony-linux/linux/go-runtime-guest/cmd/language-fixture").rglob("*.go"))
    paths.append(root / "consonance/harmony-linux/linux/go-runtime-guest/go.mod")
    paths.append(root / "workloads/bugs/historical/etcd-3.5-inconsistency/image/patches/antithesis-sdk-go-v0.8.0-linux-arm64.patch")
    paths.append(root / "workloads/languages/reviewed/go-fixture-x86_64.txt")
paths.append(root / "workloads/languages/reviewed/bookworm-x86_64.txt")
value = hashlib.sha256()
for path in sorted(paths):
    if path.is_file() and "target" not in path.parts:
        value.update(str(path.relative_to(root)).encode() + b"\0" + path.read_bytes())
print(value.hexdigest())
