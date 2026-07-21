#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail

lib=${1:?usage: check-symbols.sh path/to/libvoidstar.so}
if [ "$(uname -s)" = Darwin ]; then
    symbols=$(nm -gU "$lib")
    prefix=_
else
    symbols=$(nm -D --defined-only "$lib")
    prefix=
fi
for symbol in fuzz_json_data fuzz_get_random fuzz_flush init_coverage_module \
    notify_coverage __sanitizer_cov_trace_pc_guard_init \
    __sanitizer_cov_trace_pc_guard_internal __sanitizer_cov_trace_pc_guard; do
    if ! printf '%s\n' "$symbols" | grep -Eq "[[:space:]]${prefix}${symbol}(@@HARMONY_VOIDSTAR_1\\.0)?$"; then
        echo "FAIL: $lib does not export $symbol" >&2
        exit 1
    fi
done
echo "PASS: libvoidstar ABI symbols"
