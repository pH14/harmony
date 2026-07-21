#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# /init of the **SMB game workload image** (task 86). Bring up the kernel
# filesystems, reserve the hugetlb page the play-agent pins its billboard in,
# then run the play-agent — a headless libretro frontend driving Super Mario
# Bros. as the single supervised process. The agent draws chord inputs from the
# seeded entropy stream, emits SMB state registers, publishes the always-on
# billboard, and calls `setup_complete` (the SnapshotPoint the campaign seals
# its base at) — see harmony-linux/play-agent/.
#
# ROM discipline (task 86, hard requirement): the ROM is user-supplied at image
# build via HARMONY_SMB_ROM and never fetched. An image built without one still
# boots; this init then reports **GAME_SKIP loudly** and halts — a skipped gate
# is never a green gate.
#
# The consonance-VMM realities from pg-init.sh hold: no clock-event device wakes
# a blocked nanosleep (nothing here sleeps), and `poweroff` strands in
# device_shutdown, so terminals use the `-f` force paths:
#   * play-agent failure (rc != 0) -> `reboot -f` -> triple-fault ->
#     KVM_EXIT_SHUTDOWN -> StopReason::Crash{Shutdown} (a loud, visible failure
#     — never a silently dead workload),
#   * clean exit (a --frames bound, rc == 0) or ROM-less skip -> `halt -f` ->
#     HLT -> StopReason::Quiescent.
# In a campaign the agent normally never exits — the host stops each rollout at
# its deadline Moment.

BB=/bin/busybox

$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB mount -t tmpfs tmpfs /tmp
$BB chmod 1777 /tmp
$BB chmod 0666 /dev/console

# One 2 MiB hugetlb page for the billboard (a single contiguous guest-physical
# extent; the agent mmaps MAP_HUGETLB and publishes the gpa via state
# registers). Reserve 2 so a fragmented first reservation cannot starve it.
echo 2 >/proc/sys/vm/nr_hugepages
$BB grep -E 'HugePages_(Total|Free)' /proc/meminfo

if [ ! -f /opt/harmony/smb.nes ]; then
    echo "GAME_SKIP: no ROM in this image (HARMONY_SMB_ROM was unset at build time)"
    echo "GAME_SKIP: the game workload cannot run; provision the ROM and rebuild"
    exec $BB halt -f
fi
echo "GAME_ROM_SHA256: $($BB cat /opt/harmony/smb.nes.sha256)"

# The pre-launch marker the box harness can drive boot to before handing the
# run to the campaign (the base seal itself is the agent's setup_complete
# SnapshotPoint, after the billboard gpa/len registers are published).
echo "GAME_READY: launching play-agent"
/opt/harmony/play-agent \
    --core /opt/harmony/fceumm_libretro.so \
    --rom /opt/harmony/smb.nes
rc=$?
echo "GAME_EXIT: play-agent exited rc=$rc"

if [ "$rc" != "0" ]; then
    echo "GAME_CRASH_TERMINAL: reboot (play-agent failed)"
    exec $BB reboot -f
fi
echo "GAME_CLEAN_TERMINAL: halt"
exec $BB halt -f
