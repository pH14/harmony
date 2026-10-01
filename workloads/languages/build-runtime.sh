#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
key=$(python3 - <<'PY'
import hashlib
from pathlib import Path
value = hashlib.sha256()
for directory in ("consonance/harmony-linux/libvoidstar", "workloads/faults/runtime", "workloads/languages/runtime"):
    for path in sorted(Path(directory).rglob("*")):
        if path.is_file() and "build" not in path.parts:
            value.update(str(path).encode() + b"\0" + path.read_bytes())
print(value.hexdigest())
PY
)
tag="harmony-language-runtime:$key"
docker build --file workloads/languages/runtime/Dockerfile --tag "$tag" . >&2
printf '%s\n' "$tag"
