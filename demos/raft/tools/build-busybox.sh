#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
source_dir=${1:?pinned BusyBox source required}
output=${2:?build directory required}
mkdir -p "$output"
output=$(cd "$output" && pwd)
make -C "$source_dir" O="$output" allnoconfig >/dev/null
for symbol in STATIC BUSYBOX ASH SH_IS_ASH ASH_JOB_CONTROL FEATURE_EDITING MOUNT UMOUNT MKDIR MKNOD CHMOD CHOWN CAT ECHO GREP HALT POWEROFF REBOOT SETSID CTTYHACK GZIP GUNZIP ZCAT BASE64 ENV ID KILL SLEEP LN RM CP MV TRUE FALSE TEST SYNC PRINTF HEAD TAIL TEE CUT WC PS SED TOUCH STAT READLINK MKFIFO TEST1 UNAME STTY FEATURE_MOUNT_FLAGS FEATURE_STAT_FORMAT FEATURE_SH_MATH FEATURE_SH_MATH_64; do
    sed -i "s/^# CONFIG_${symbol} is not set$/CONFIG_${symbol}=y/" "$output/.config"
done
set +o pipefail
yes '' | make -C "$source_dir" O="$output" oldconfig >/dev/null
set -o pipefail
make -C "$source_dir" O="$output" -j"$(nproc)" busybox >/dev/null
for app in sh mount mkdir printf setsid cttyhack stty; do
    "$output/busybox" --list | grep -qx "$app"
done
