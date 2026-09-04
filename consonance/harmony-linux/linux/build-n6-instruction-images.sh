#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build table-generated N6 JIT instruction-sweep initramfses and their
# traps-off planted negatives for the native Linux architecture.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

n6_cc=${HARMONY_N6_CC:-cc}
require_tools "$n6_cc" cc gzip objdump python3
extract_kernel # usr/gen_init_cpio.c is the deterministic packer.

repo_root=$(cd ../../.. && pwd)
generator=$repo_root/consonance/harmony-linux/scripts/n6-instruction-sweep.py
entropy_scan=$repo_root/consonance/harmony-linux/scripts/n6-entropy-scan.py
table=$repo_root/consonance/acceptance-suite/instruction-contract.toml
n6_root=$BUILD_ROOT/n6-instruction-sweep
mkdir -p "$n6_root"

case "$(uname -m)" in
    aarch64)
        arch=arm64
        artifact_root=$ARM64_ART_DIR
        output=initramfs-n6.cpio.gz
        traps_output=initramfs-n6-traps-off.cpio.gz
        march=(-march=armv8.1-a+lse -mno-outline-atomics)
        negative_body='.inst 0xd53b2400
.inst 0xd53b2420'
        ;;
    x86_64)
        arch=x86_64
        artifact_root=$ART_DIR
        output=initramfs-n6.cpio.gz
        traps_output=initramfs-n6-traps-off.cpio.gz
        march=()
        negative_body='.byte 0x0f, 0xc7, 0xf0
.byte 0x0f, 0xc7, 0xf8'
        false_positive_body='.byte 0x48, 0x8d, 0x3d, 0x0f, 0xc7, 0xf8, 0x00'
        ;;
    *)
        echo "FAIL: N6 guest images require native Linux/aarch64 or Linux/x86_64" >&2
        exit 1
        ;;
esac

echo "== N6: generator/verifier planted negatives"
python3 "$generator" --table "$table" self-test
python3 "$generator" --table "$table" guest-assembly --arch "$arch" \
    >"$n6_root/n6-generated.S"
python3 "$generator" --table "$table" guest-header --arch "$arch" \
    >"$n6_root/n6-generated.h"

echo "== N6: entropy audit planted negative"
negative=$n6_root/n6-entropy-negative.S
cat >"$negative" <<EOF
.text
.global _start
_start:
$negative_body
ret
EOF
"$n6_cc" -nostdlib -static -Wl,-e,_start -o "$n6_root/n6-entropy-negative" "$negative"
if python3 "$entropy_scan" "$n6_root/n6-entropy-negative" \
    >"$n6_root/n6-entropy-negative.log" 2>&1; then
    echo "FAIL: entropy scanner accepted the planted $arch opcodes" >&2
    exit 1
fi
if ! grep -q '^N6_ENTROPY_REJECT .* hits=2$' "$n6_root/n6-entropy-negative.log"; then
    echo "FAIL: entropy scanner did not identify both planted opcodes" >&2
    cat "$n6_root/n6-entropy-negative.log" >&2
    exit 1
fi
echo "ok: planted entropy opcodes rejected"

if [ "$arch" = x86_64 ]; then
    false_positive=$n6_root/n6-entropy-false-positive.S
    cat >"$false_positive" <<EOF
.text
.global _start
_start:
$false_positive_body
ret
EOF
    "$n6_cc" -nostdlib -static -Wl,-e,_start \
        -o "$n6_root/n6-entropy-false-positive" "$false_positive"
    python3 "$entropy_scan" "$n6_root/n6-entropy-false-positive"
    echo "ok: entropy scanner ignored opcode-shaped LEA displacement"
fi

build_guest() {
    mode=$1
    binary=$2
    extra=()
    if [ "$mode" = traps-off ]; then
        extra=(-DN6_TRAPS_OFF=1)
    fi
    "$n6_cc" -Os -Wall -Wextra -Werror -ffunction-sections -fdata-sections \
        -static -fno-pie -no-pie -Wl,--build-id=none -Wl,-z,noexecstack \
        -Wl,--gc-sections "${march[@]}" \
        -I"$n6_root" -DN6_ARCH='"'"$arch"'"' -DN6_AUDIT_REJECTED=1 \
        "${extra[@]}" -o "$binary" \
        "$LINUX_DIR/n6-instruction-guest.c" "$n6_root/n6-generated.S"
    python3 "$entropy_scan" "$binary"
}

echo "== N6: compile table-generated JIT guests"
build_guest traps-on "$n6_root/n6-instruction-guest"
build_guest traps-off "$n6_root/n6-instruction-guest-traps-off"

cc -O2 -o "$n6_root/gen_init_cpio" "$KSRC/usr/gen_init_cpio.c"
mkdir -p "$artifact_root"
pack() {
    binary=$1
    name=$2
    spec=$n6_root/$name.spec
    cat >"$spec" <<EOF
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
nod /dev/kmsg 0600 0 0 c 1 11
file /init $binary 0755 0 0
EOF
    "$n6_root/gen_init_cpio" -t 0 "$spec" | gzip -n -9 >"$artifact_root/$name"
    echo "ok: $artifact_root/$name"
}

pack "$n6_root/n6-instruction-guest" "$output"
pack "$n6_root/n6-instruction-guest-traps-off" "$traps_output"
