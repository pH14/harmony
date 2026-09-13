#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
apply=$(cd "$(dirname "$0")" && pwd)/apply-patch-series.sh
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/tree" "$scratch/common" "$scratch/arch"
printf 'one\n' > "$scratch/tree/value"
cat > "$scratch/common/0001-first.patch" <<'PATCH'
--- a/value
+++ b/value
@@ -1 +1 @@
-one
+two
PATCH
cat > "$scratch/arch/0001-second.patch" <<'PATCH'
--- a/value
+++ b/value
@@ -1 +1 @@
-two
+three
PATCH
bash "$apply" "$scratch/tree" "$scratch/common" "$scratch/arch"
bash "$apply" "$scratch/tree" "$scratch/common" "$scratch/arch"
[ "$(cat "$scratch/tree/value")" = three ]
printf '\n' >> "$scratch/arch/0001-second.patch"
if bash "$apply" "$scratch/tree" "$scratch/common" "$scratch/arch"; then
    echo 'FAIL: changed patch accepted' >&2; exit 1
fi
[ "$(cat "$scratch/tree/value")" = three ]
mkdir "$scratch/partial"
printf 'unexpected\n' > "$scratch/partial/value"
if bash "$apply" "$scratch/partial" "$scratch/common" "$scratch/arch"; then
    echo 'FAIL: incompatible tree accepted' >&2; exit 1
fi
[ -f "$scratch/partial/.harmony-patch-series.pending" ]
printf 'one\n' > "$scratch/partial/value"
if bash "$apply" "$scratch/partial" "$scratch/common" "$scratch/arch"; then
    echo 'FAIL: incomplete series accepted' >&2; exit 1
fi
[ "$(cat "$scratch/partial/value")" = one ]
echo 'patch-series regression checks passed'
