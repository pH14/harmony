#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
set -eu
cpu=${1:?usage: check-code-generator.sh JDK-SOURCE}/src/hotspot/cpu
fail() {
    echo "check-code-generator: $*" >&2
    exit 1
}
if grep -rniwE 'rdrand|rdseed|rndr|rndrrs' "$cpu/x86" "$cpu/aarch64" | grep -v '/vm_version_x86\.cpp:'; then
    fail "HotSpot names an entropy instruction"
fi
sites=$(grep -rnE '0x0F.*0xC7' "$cpu/x86" | grep -v 'macroAssembler_x86\.hpp:' || true)
[ "$(printf '%s\n' "$sites" | grep -c 'emit_int16(0x0F, (unsigned char)0xC7)')" = 1 ] \
    || fail "unexpected 0F C7 encodings: $sites"
grep -B3 'emit_int16(0x0F, (unsigned char)0xC7)' "$cpu/x86/assembler_x86.cpp" | grep -q 'void Assembler::cmpxchg8' \
    || fail "the 0F C7 encoding is outside cmpxchg8"
grep -A1 'emit_int16(0x0F, (unsigned char)0xC7)' "$cpu/x86/assembler_x86.cpp" | grep -q 'emit_operand(rcx,' \
    || fail "cmpxchg8 no longer encodes /1"
if grep -rnE 'mrs\([^,]+, *0b0010, *0b0100,' "$cpu/aarch64"; then
    fail "HotSpot reads RNDR or RNDRRS"
fi
if grep -rniE 'd53b24' "$cpu/aarch64"; then
    fail "HotSpot encodes an RNDR read"
fi
echo "check-code-generator: no entropy encodings in HotSpot"
