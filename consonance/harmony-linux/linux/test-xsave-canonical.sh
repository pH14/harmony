#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
task_dir=$(mktemp -d)
trap 'rm -rf "$task_dir"' EXIT
python3 - "$here/patches/x86/0008-x86-harmony-canonical-xsave.patch" "$task_dir/harmony_xstate.h" <<'PY'
from pathlib import Path
import sys
text = Path(sys.argv[1]).read_text()
part = text.split('+++ b/arch/x86/kernel/fpu/harmony_xstate.h\n', 1)[1]
part = part.split('\n--- a/', 1)[0]
lines = [line[1:] for line in part.splitlines() if line.startswith('+')]
Path(sys.argv[2]).write_text('\n'.join(lines) + '\n')
PY
"${CC:-cc}" -std=gnu11 -O2 -Wall -Wextra -Werror -I"$task_dir" \
    "$here/test-xsave-canonical.c" -o "$task_dir/test"
"$task_dir/test"
