#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build the **exec-capable** initramfs (task 81): static busybox + `exec-init.sh`
# as /init (an interactive root shell on ttyS0), packed reproducibly with the
# kernel's own gen_init_cpio exactly like `build-initramfs.sh`. Produces
# guest/build/initramfs-exec.cpio.gz — the image the `exec` improvisation box gate
# (`consonance/vmm-core/tests/live_exec_improvisation.rs`, with EXEC_EXPECT_OUTPUT=1)
# talks a real command to. Reuses the kernel built by `make kernel`.
#
# The ONLY difference from `build-initramfs.sh` is the /init source (exec-init.sh
# vs init.sh); everything else — the static busybox, the reproducible packing (-t 0,
# gzip -n), the spec layout — is identical, so the image is byte-reproducible.
set -euo pipefail

cd "$(dirname "$0")"

# shellcheck source=lib-build.sh disable=SC1091
. ./lib-build.sh

require_linux_amd64
require_tools cc make gzip bzip2
extract_busybox
extract_kernel # for usr/gen_init_cpio.c

mkdir -p "$BBOBJ" "$ART_DIR"

echo "== exec-image: building static busybox ($BUSYBOX_VERSION)"
make -C "$BBSRC" O="$BBOBJ" defconfig
sed -e 's/^# CONFIG_STATIC is not set$/CONFIG_STATIC=y/' \
    -e 's/^CONFIG_TC=y$/# CONFIG_TC is not set/' \
    "$BBOBJ/.config" >"$BBOBJ/.config.tmp"
mv "$BBOBJ/.config.tmp" "$BBOBJ/.config"
set +o pipefail
yes '' | make -C "$BBSRC" O="$BBOBJ" oldconfig >/dev/null
set -o pipefail
if ! grep -qxF 'CONFIG_STATIC=y' "$BBOBJ/.config"; then
    echo "FAIL: CONFIG_STATIC=y did not stick in the busybox config" >&2
    exit 1
fi
make -C "$BBSRC" O="$BBOBJ" -j"$(nproc)" busybox

# --- the G3 busy-wait: static pvclock-spin -----------------------------------
# G3's guest must spin on the clock page with NO syscalls and NO raw counter
# reads in the loop — a shell `date` loop syscalls, and this kernel's syscall
# entry carries an rdtsc (kstack randomization), which is a V-time intercept
# that refreshes the page for free and makes the gate vacuous (cross-model r5).
# See pvclock-spin.c. Static + -O2, like campaign-super.
echo "== exec-image: compiling static pvclock-spin (G3's syscall-free busy-wait)"
cc -static -O2 -Wall -Wextra -fno-asynchronous-unwind-tables \
    -o "$BUILD_ROOT/pvclock-spin" "$LINUX_DIR/pvclock-spin.c"
[ -x "$BUILD_ROOT/pvclock-spin" ] || { echo "FAIL: pvclock-spin did not build" >&2; exit 1; }

echo "== exec-image: packing with gen_init_cpio"
cc -O2 -o "$BUILD_ROOT/gen_init_cpio" "$KSRC/usr/gen_init_cpio.c"

spec=$BUILD_ROOT/initramfs-exec.spec
cat >"$spec" <<EOF
dir /dev 0755 0 0
nod /dev/console 0600 0 0 c 5 1
nod /dev/mem 0600 0 0 c 1 1
dir /proc 0755 0 0
dir /sys 0755 0 0
dir /bin 0755 0 0
file /bin/busybox $BBOBJ/busybox 0755 0 0
slink /bin/sh /bin/busybox 0777 0 0
file /bin/pvclock-spin $BUILD_ROOT/pvclock-spin 0755 0 0
file /init $LINUX_DIR/exec-init.sh 0755 0 0
EOF

"$BUILD_ROOT/gen_init_cpio" -t 0 "$spec" | gzip -n -9 >"$ART_DIR/initramfs-exec.cpio.gz"
echo "ok: $ART_DIR/initramfs-exec.cpio.gz"
echo "note: record its sha256 in MANIFEST.sha256 per the task-90 hashed-input ruling if this"
echo "      image becomes a gated artifact (it is off-record test scaffolding for now)."
