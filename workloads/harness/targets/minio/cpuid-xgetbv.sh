#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Admission accepts XGETBV only where the code provably clears ECX first.
# klauspost/cpuid loads ECX from its argument, and every caller passes 0, so
# the module is replaced by a copy that clears ECX in place of that load.
set -euo pipefail
module=github.com/klauspost/cpuid/v2
version=$(go list -m -f "{{.Version}}" "${module}" 2>/dev/null || true)
[ -n "${version}" ] || exit 0
copy=${1:?usage: cpuid-xgetbv.sh copy-directory}
rm -rf "${copy}"
cp -r "$(go env GOMODCACHE)/${module}@${version}" "${copy}"
chmod -R u+w "${copy}"
if grep -h -E "[^a-zA-Z]xgetbv\\(" "${copy}"/*.go | grep -v -E "xgetbv\\(0\\)|func\\(uint32\\)|xgetbv func"; then
    echo "xgetbv called with a selector other than 0" >&2
    exit 1
fi
sed -i "s/^\tMOVL index+0(FP), CX$/\tXORL CX, CX/" "${copy}/cpuid_amd64.s"
grep -q "^	XORL CX, CX$" "${copy}/cpuid_amd64.s"
go mod edit -replace "${module}=${copy}"
