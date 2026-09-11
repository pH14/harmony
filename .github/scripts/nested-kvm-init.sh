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
done < /modules.txt || status=1
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
if [ -f /expected-snapshot-tests ] && [ "$status" -eq 0 ]; then
    mkdir -p /reports || status=1
    export XSAVE_CONTINUATION_REPORT_DIR=/reports/xsave-continuation
    export XSAVE_LIVE_REGISTERS_REPORT_DIR=/reports/xsave-live-registers
    export LEAF_EXPECT_SYNC_SHADOW=Y
    for case in pae_cached_pdptrs_survive_full_vmm_snapshot_restore \
        mmio_rmw_finishes_before_full_vmm_snapshot \
        xsave_guest_bytes_survive_cold_continuation \
        xsave_live_registers_survive_cold_continuation \
        shadow_leaf_host_write_snapshot_observations \
        shadow_leaf_guest_write_snapshot_observations; do
        export LEAF_REPORT_DIR="/reports/$case"
        /bin/x86_cpu_snapshots "live_kvm::$case" --exact --ignored \
            --test-threads=1 --nocapture > "/reports/$case.log" 2>&1 || status=1
        cat "/reports/$case.log" || status=1
        grep -q '^test result: ok. 1 passed; 0 failed; 0 ignored;' \
            "/reports/$case.log" || status=1
    done
    if tar -czf /tmp/snapshot-evidence.tar.gz -C /reports .; then
        size=$(wc -c < /tmp/snapshot-evidence.tar.gz)
        if [ "$size" -le 16777216 ]; then
            printf 'SNAPSHOT_EVIDENCE_BEGIN\n'
            base64 /tmp/snapshot-evidence.tar.gz || status=1
            printf 'SNAPSHOT_EVIDENCE_END\n'
        else
            printf 'Snapshot evidence exceeds 16 MiB compressed limit\n'
            status=1
        fi
    else
        status=1
    fi
    printf 'INNER_SNAPSHOT_TESTS_STATUS=%s\n' "$status"
fi
printf 'INNER_PROBE_STATUS=%s\n' "$status"
poweroff -f
while :; do sleep 1; done
