#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# /init of the flow-agent DOORBELL-FIRING image (hm-rdp): the minimal vehicle for
# the first-ever live validation that the flow-agent's privileged doorbell path —
# iopl(3) + an /dev/mem mmap of the fixed REQ/RESP hypercall pages + the OUT — now
# executes on the doorbell-capable guest kernel (CONFIG_X86_IOPL_IOPERM=y,
# e60ff83; CONFIG_DEVMEM=y, 48fb632), which it could never have done on the older
# config those flags postdate.
#
# `--dry-run` rings `net_decide` over the real doorbell but installs no `tc`/`nft`,
# so no CNI is required. The base-seal driver (`campaign-runner box --kernel
# <game/maze bzImage> --initramfs initramfs-flow.cpio.gz --ready-marker FLOW_DONE`)
# streams the agent's serial during the boot drive. If the host has not yet wired
# its Net service — the doorbell fires during `drive_to_marker`, BEFORE
# `ControlServer::new`'s `enable_net` (the F10 ordering, hm-i8kc) — the agent logs
# `Net doorbell unwired -> nominal`, a clean deterministic fallback; the guest-side
# path still executed (the point). A real `net_decide` answer needs the agent to
# run mid-served-workload (the k3s image path, hm-wvh) or an F10 wiring fix.
#
# The consonance-VMM force terminals hold (see maze-init.sh), SUCCESS-GATED so the
# gate cannot pass on a failed agent (PR161-F1): a flow-agent failure (rc != 0) ->
# `reboot -f` BEFORE the FLOW_DONE marker -> the triple-fault reaches
# drive_to_marker as Step::Terminal -> boot_server FAILURE (loud AND fatal). A
# clean run (rc == 0, including the unwired-host `-> nominal` fallback, which is a
# successful doorbell round-trip) emits FLOW_DONE -> `halt -f` -> HLT ->
# StopReason::Quiescent. Emitting the marker unconditionally would seal the base at
# a point the failure already passed and print a vacuous GATES PASS.
BB=/bin/busybox

$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB mount -t tmpfs tmpfs /tmp
$BB chmod 1777 /tmp
$BB chmod 0666 /dev/console 2>/dev/null

echo "FLOW_LAUNCH: firing the flow-agent doorbell (hm-rdp first-ever live validation)"
if [ -e /dev/mem ]; then echo "FLOW_DEVMEM: present"; else echo "FLOW_DEVMEM: ABSENT (CONFIG_DEVMEM missing)"; fi
if [ -e /dev/urandom ]; then echo "FLOW_URANDOM: present"; else echo "FLOW_URANDOM: ABSENT"; fi

# A first vertical single flow; --dry-run so the doorbell fires without a CNI.
/opt/harmony/flow-agent --dry-run --src 1 --dst 2 --conn 1 \
    --iface lo --dst-ip 127.0.0.1 --dport 5432
rc=$?
echo "FLOW_AGENT_RC=$rc"
# Success-gate the marker (PR161-F1): FLOW_DONE — the sweep's --ready-marker — is
# emitted ONLY on rc == 0. A failed agent reboots FIRST, so the marker never
# appears; drive_to_marker then reaches the triple-fault as a terminal and
# boot_server fails loudly, instead of sealing past the failure and printing a
# vacuous GATES PASS (the VTIM reseed fold alone satisfies the sweep's
# >=2-distinct-futures check with zero guest entropy, so a green must not be
# trusted on a Crash-path run).
if [ "$rc" != "0" ]; then
    echo "FLOW_FAILED: reboot (flow-agent failed)"
    exec $BB reboot -f
fi
echo "FLOW_DONE"
echo "FLOW_CLEAN_TERMINAL: halt"
exec $BB halt -f
