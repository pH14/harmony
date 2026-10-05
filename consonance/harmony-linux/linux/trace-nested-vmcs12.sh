#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
set -euo pipefail
tracing=${2:-/sys/kernel/tracing}
events=/sys/kernel/tracing/kprobe_events

offset() {
    pahole -C "$1" --btf_base /sys/kernel/btf/vmlinux /sys/kernel/btf/kvm_intel 2>/dev/null |
        awk -v member="$2;" '{ for (i = 1; i < NF; i++) if ($i == member) { print $(i + 2); exit } }'
}

case ${1:?usage: trace-nested-vmcs12.sh start|stop [TRACEFS_INSTANCE]} in
start)
    nested=$(offset vcpu_vmx nested)
    field() { echo $((nested + $(offset nested_vmx "$1"))); }
    vmcs12=$(field cached_vmcs12)
    fetch="dirty=+$(field dirty_vmcs12)(\$arg1):u8"
    fetch+=" rare=+$(field need_sync_vmcs02_to_vmcs12_rare)(\$arg1):u8"
    fetch+=" initialized=+$(field vmcs02_initialized)(\$arg1):u8"
    fetch+=" low=+0(+$vmcs12(\$arg1)):x64[64] high=+512(+$vmcs12(\$arg1)):x64[64]"
    printf '%s\n' "p:harmony_nested/vmcs12_enter kvm_intel:nested_vmx_enter_non_root_mode $fetch" >>"$events"
    printf 1 >"$tracing/events/harmony_nested/enable"
    ;;
stop)
    printf 0 >"$tracing/events/harmony_nested/enable"
    printf '%s\n' -:harmony_nested/vmcs12_enter >>"$events"
    ;;
esac
