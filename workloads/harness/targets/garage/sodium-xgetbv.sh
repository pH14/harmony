#!/bin/bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Admission accepts XGETBV only after a straight-line zero selector. The bundled
# libsodium reuses a zero register set before a branch, so its one XGETBV is
# rewritten to clear ECX in the same instruction sequence.
set -euo pipefail

files=("${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/libsodium-sys-*/libsodium/src/libsodium/sodium/runtime.c)
if [ "${#files[@]}" -ne 1 ] || [ ! -f "${files[0]}" ]; then
    echo "expected one libsodium runtime.c, found: ${files[*]}" >&2
    exit 1
fi
file="${files[0]}"
if grep -rl -i -e xgetbv -e '0x01, 0xd0' "$(dirname "$(dirname "$file")")" | grep -v -x "$file"; then
    echo "XGETBV outside runtime.c" >&2
    exit 1
fi
before="$(wc -l < "$file")"
perl -0pi -e 's/(\n[ \t]+xcr0 = 0U;\n).*?(\n[ \t]*if \(\(xcr0 & \(XCR0_SSE \| XCR0_AVX\)\))/$1        __asm__ __volatile__("xorl %%ecx, %%ecx\\n\\t.byte 0x0f, 0x01, 0xd0" : "=a"(xcr0) : : "%ecx", "%edx");$2/s' "$file"
test "$(grep -c '0x01, 0xd0' "$file")" -eq 1
test $((before - $(wc -l < "$file"))) -le 30
grep -q 'xorl %%ecx, %%ecx' "$file"
if grep -q '_xgetbv' "$file"; then
    echo "_xgetbv still present in $file" >&2
    exit 1
fi
