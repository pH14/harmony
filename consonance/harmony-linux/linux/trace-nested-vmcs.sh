#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
tracing=${2:-/sys/kernel/tracing}
events=/sys/kernel/tracing/kprobe_events

offset() {
    pahole -C "$1" --btf_base /sys/kernel/btf/vmlinux /sys/kernel/btf/kvm_intel 2>/dev/null |
        awk -v member="$2;" '{ for (i = 1; i < NF; i++) if ($i == member) { print $(i + 2); exit } }'
}

probe() {
    printf '%s\n' "p:harmony_nested/$1 kvm_intel:$2 $3" >>"$events" ||
        echo "probe $1 on $2 unavailable" >&2
}

filter() {
    if [[ -d $tracing/events/harmony_nested/$1 ]]; then
        printf '%s\n' "$2" >"$tracing/events/harmony_nested/$1/filter"
    fi
}

case ${1:?usage: trace-nested-vmcs.sh start|stop [TRACEFS_INSTANCE]} in
start)
    nested=$(offset vcpu_vmx nested)
    field() { echo $((nested + $(offset nested_vmx "$1"))); }
    vmcs12=$(field cached_vmcs12)
    fetch="dirty=+$(field dirty_vmcs12)(\$arg1):u8"
    fetch+=" rare=+$(field need_sync_vmcs02_to_vmcs12_rare)(\$arg1):u8"
    fetch+=" initialized=+$(field vmcs02_initialized)(\$arg1):u8"
    fetch+=" low=+0(+$vmcs12(\$arg1)):x64[64] high=+512(+$vmcs12(\$arg1)):x64[64]"
    probe vmcs12_enter nested_vmx_enter_non_root_mode "$fetch"

    vcpu_arch=$(offset kvm_vcpu arch)
    arch() { echo $((vcpu_arch + $(offset kvm_vcpu_arch "$1"))); }
    loaded=$(offset vcpu_vmx loaded_vmcs)
    queued="hflags=+$(arch hflags)(\$arg1):x32"
    queued+=" irq=+$(arch interrupt)(\$arg1):x32"
    queued+=" exception=+$(arch exception)(\$arg1):x64"
    queued+=" nmi=+$(arch nmi_injected)(\$arg1):u8"
    vmcs="low=+0(+0(+$loaded(\$arg1))):x64[64] high=+512(+0(+$loaded(\$arg1))):x64[64]"
    probe vmcs02_run vmx_vcpu_run "$queued $vmcs"
    reason=$(offset vcpu_vmx exit_reason)
    reason=${reason:-$(($(offset vcpu_vmx vt) + $(offset vcpu_vt exit_reason)))}
    probe vmcs02_exit vmx_handle_exit "reason=+$reason(\$arg1):x32 $queued $vmcs"
    for inject in irq nmi exception; do
        probe "inject_$inject" "vmx_inject_$inject" "$queued"
        filter "inject_$inject" 'hflags & 1'
    done
    filter vmcs02_run 'hflags & 1'
    filter vmcs02_exit 'reason & 0x80000000'
    printf 1 >"$tracing/events/harmony_nested/enable"
    ;;
stop)
    printf 0 >"$tracing/events/harmony_nested/enable"
    for event in "$tracing"/events/harmony_nested/*/; do
        printf '%s\n' "-:harmony_nested/$(basename "$event")" >>"$events"
    done
    ;;
esac
