#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
export PATH=/bin
status=0
mount -t proc proc /proc || status=1
mount -t sysfs sysfs /sys || status=1
mount -t devtmpfs devtmpfs /dev || status=1
printf 'CONTAINING_KERNEL='
uname -r
while read -r module; do
    insmod "$module" || status=1
done < /modules.txt
if [ -f /expected-sync-shadow ]; then
    sync=$(cat /sys/module/kvm/parameters/harmony_sync_shadow) || status=1
    printf 'INNER_SYNC_SHADOW=%s\n' "$sync"
    [ "$sync" = Y ] || status=1
    if [ -r /sys/module/kvm_amd/parameters/npt ]; then
        paging=$(cat /sys/module/kvm_amd/parameters/npt) || status=1
    else
        paging=$(cat /sys/module/kvm_intel/parameters/ept) || status=1
    fi
    printf 'INNER_HARDWARE_PAGING=%s\n' "$paging"
    [ "$paging" = N ] || status=1
fi
if [ "$status" -eq 0 ]; then
    /bin/nested-kvm-hlt || status=$?
fi
printf 'INNER_PROBE_STATUS=%s\n' "$status"
poweroff -f
while :; do sleep 1; done
