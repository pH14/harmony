#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
harmony=${1:?harmony binary required}
profile=${2:?UML profile required}
base=${3:?OCI base initramfs required}
busybox=${4:?static BusyBox required}
image=${5:?container image required}
out=${6:?output directory required}
repo=$(cd "$(dirname "$0")/../../.." && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
mkdir -p "$root"/{bin,lib,lib64,proc,sys,dev,tmp,run,demo,licenses}
cp "$busybox" "$root/bin/busybox"
while IFS= read -r app; do ln -s busybox "$root/bin/$app"; done < <("$busybox" --list)
cp "$harmony" "$root/bin/harmony"
cp /usr/bin/tar "$root/bin/tar"
while IFS= read -r library; do
    mkdir -p "$root$(dirname "$library")"
    cp -L "$library" "$root$library"
done < <(ldd "$harmony" /usr/bin/tar | awk '{for(i=1;i<=NF;i++)if($i~/^\// && $i!~/:$/)print $i}')
cp -a "$profile" "$root/demo/uml"
cp "$base" "$root/demo/base.cpio.gz"
podman save --format oci-dir --output "$root/demo/image" "$image"
cp "$repo/demos/raft/workload/scenario.json" "$root/demo/actions.json"
cat > "$root/demo/harmony.toml" <<'CONFIG'
[workload]
package = "faults"
input = "/demo/image"
[runner]
kind = "consonance"
backend = "uml"
[runner.options]
ram_mib = 128
uml_profile = "/demo/uml"
base_initramfs = "/demo/base.cpio.gz"
[search]
seed = 0
executions = 1
CONFIG
cat > "$root/init" <<'INIT'
#!/bin/sh
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev
mkdir -p /dev/pts /dev/shm /tmp /run
mount -t devpts devpts /dev/pts
mount -t tmpfs tmpfs /dev/shm
export PATH=/bin
export HOME=/demo
export TERM=xterm
export PS1="harmony-linux# "
cd /demo
printf '\nHARMONY_BROWSER_READY\nReal Linux + Consonance UML + Harmony CLI.\n'
exec setsid cttyhack sh
INIT
chmod +x "$root/init"
cp /usr/share/doc/libc6/copyright "$root/licenses/host-libc6.txt"
cp /usr/share/doc/libgcc-s1/copyright "$root/licenses/host-libgcc.txt"
cp "$repo/LICENSE" "$root/licenses/Harmony.txt"
cp "$repo/demos/raft/workload/raft.c" "$root/demo/raft.c"
(cd "$root" && find . -print0 | LC_ALL=C sort -z | cpio --null -o -H newc 2>/dev/null | gzip -n -6) > "$out/browser.cpio.gz"
sha256sum "$out/browser.cpio.gz"
