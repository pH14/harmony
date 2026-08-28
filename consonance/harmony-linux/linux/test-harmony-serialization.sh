#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build and boot the /dev/harmony KUnit concurrency test, then repeat with the
# serialization helper deliberately mutated to the pre-fix no-lock behavior.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools make perl qemu-system-x86_64

if [ ! -f "$KOBJ/.config" ] || [ ! -f "$ART_DIR/initramfs.cpio.gz" ]; then
    echo "FAIL: build the x86 kernel and initramfs before the serialization gate" >&2
    exit 1
fi

driver=$KSRC/drivers/misc/harmony.c
backup=$BUILD_ROOT/harmony.c.n4-serialization-backup
positive_obj=$BUILD_ROOT/kernel-build-harmony-kunit
negative_obj=$BUILD_ROOT/kernel-build-harmony-kunit-no-lock
positive_log=$BUILD_ROOT/harmony-kunit-positive.log
negative_log=$BUILD_ROOT/harmony-kunit-negative.log

restore_driver() {
    if [ -f "$backup" ]; then
        mv "$backup" "$driver"
    fi
}
trap restore_driver EXIT

configure_kunit() {
    object_dir=$1
    rm -rf "$object_dir"
    mkdir -p "$object_dir"
    cp "$KOBJ/.config" "$object_dir/.config"
    "$KSRC/scripts/config" --file "$object_dir/.config" \
        -e KUNIT -e KUNIT_TEST -e KUNIT_DEFAULT_ENABLED \
        -e HARMONY_DEVICE_KUNIT_TEST
    make -C "$KSRC" O="$object_dir" ARCH=x86_64 olddefconfig
    if ! grep -qxF 'CONFIG_HARMONY_DEVICE_KUNIT_TEST=y' "$object_dir/.config"; then
        echo "FAIL: Harmony KUnit test did not survive olddefconfig" >&2
        exit 1
    fi
}

build_kunit_kernel() {
    object_dir=$1
    make -C "$KSRC" O="$object_dir" ARCH=x86_64 LOCALVERSION= \
        -j"$(nproc)" bzImage
}

boot_kunit_kernel() {
    object_dir=$1
    log=$2
    timeout -k 10 180 qemu-system-x86_64 \
        -m 512 -nographic -no-reboot -machine hpet=off \
        -kernel "$object_dir/arch/x86/boot/bzImage" \
        -initrd "$ART_DIR/initramfs.cpio.gz" \
        -append 'console=ttyS0 panic=-1 random.trust_cpu=off kunit.enable=1' \
        </dev/null >"$log" 2>&1
}

echo "== harmony serialization: fixed driver must pass"
configure_kunit "$positive_obj"
build_kunit_kernel "$positive_obj"
boot_kunit_kernel "$positive_obj" "$positive_log"
if ! grep -Eq 'ok [0-9]+( -)? harmony-transaction-lock[[:space:]]*$' "$positive_log"; then
    echo "FAIL: fixed-driver KUnit suite did not pass" >&2
    tail -80 "$positive_log" >&2
    exit 1
fi
echo "ok: fixed driver held the second ringer outside the in-flight exchange"

echo "== harmony serialization: pre-fix no-lock mutant must fail"
cp "$driver" "$backup"
perl -0pi -e 's/\tint ret;\n\n\tif \(mutex_lock_interruptible\(&harmony_lock\)\)\n\t\treturn -ERESTARTSYS;\n\tret = function\(context\);\n\tmutex_unlock\(&harmony_lock\);\n\treturn ret;/\treturn function(context);/' "$driver"
if grep -A10 'static int harmony_run_serialized' "$driver" \
    | grep -q 'mutex_lock_interruptible'; then
    echo "FAIL: no-lock mutation did not remove the serialization helper lock" >&2
    exit 1
fi
configure_kunit "$negative_obj"
build_kunit_kernel "$negative_obj"
boot_kunit_kernel "$negative_obj" "$negative_log"
if ! grep -Eq 'not ok [0-9]+( -)? harmony_concurrent_ringer_test[[:space:]]*$' "$negative_log"; then
    echo "FAIL: concurrency test did not reject the pre-fix no-lock mutant" >&2
    tail -80 "$negative_log" >&2
    exit 1
fi
echo "ok: pre-fix no-lock mutant was rejected by the concurrent-ringer test"
echo "PASS: /dev/harmony serialization positive and negative controls"
