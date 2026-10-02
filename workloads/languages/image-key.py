#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Hash the inputs to a language layer, independently of the runtime."""

import hashlib
import sys
from pathlib import Path

VENDOR = "workloads/languages/vendor"
FORWARDING = [
    f"{VENDOR}/antithesis_instrumentation.h",
    f"{VENDOR}/ANTITHESIS-SDK-CPP-LICENSE",
    "workloads/languages/c/shim.c",
]
EXTRA_INPUTS = {
    "c": FORWARDING + ["workloads/faults/runtime/tests/language_fixture.c"],
    "rust": ["workloads/faults/runtime/tests/language_fixture.rs"],
    "go": [
        "consonance/harmony-linux/linux/go-runtime-guest/go.mod",
        "consonance/harmony-linux/linux/go-runtime-guest/cmd/language-fixture/main.go",
        "workloads/bugs/historical/etcd-3.5-inconsistency/image/patches/antithesis-sdk-go-v0.8.0-linux-arm64.patch",
    ],
    "python": FORWARDING,
    "java": FORWARDING,
}

root = Path(__file__).resolve().parents[2]
language = sys.argv[1]
if language not in EXTRA_INPUTS:
    raise SystemExit(f"unsupported language: {language}")
paths = [path for path in (root / "workloads/languages" / language).rglob("*") if path.is_file() and path.suffix != ".md"]
paths.extend(root / path for path in EXTRA_INPUTS[language])
value = hashlib.sha256()
for path in sorted(paths):
    if "target" not in path.relative_to(root).parts:
        value.update(str(path.relative_to(root)).encode() + b"\0" + path.read_bytes())
print(value.hexdigest())
