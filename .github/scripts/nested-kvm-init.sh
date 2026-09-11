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
if [ "$status" -eq 0 ]; then
    /bin/nested-kvm-hlt || status=$?
fi
printf 'INNER_PROBE_STATUS=%s\n' "$status"
poweroff -f
while :; do sleep 1; done
