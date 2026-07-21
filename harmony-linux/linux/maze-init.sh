#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# /init of the **maze workload image** (task 134). Bring up the kernel
# filesystems, then run the maze agent — the deterministic gauntlet walk as
# the single supervised process. The agent draws one entropy byte per step
# from the seeded stream, emits its X/Y position registers, marks the goal
# with `assert_reachable`, and calls `setup_complete` (the SnapshotPoint the
# campaign seals its base at) — see harmony-linux/maze-agent/. Zero fault
# vocabulary; no ROM, no billboard, no hugetlb.
#
# The consonance-VMM realities from pg-init.sh hold: no clock-event device
# wakes a blocked nanosleep (nothing here sleeps), and `poweroff` strands in
# device_shutdown, so terminals use the `-f` force paths:
#   * maze-agent failure (rc != 0) -> `reboot -f` -> triple-fault ->
#     KVM_EXIT_SHUTDOWN -> StopReason::Crash{Shutdown} (a loud, visible
#     failure — never a silently dead workload),
#   * clean exit (a --steps bound, rc == 0) -> `halt -f` -> HLT ->
#     StopReason::Quiescent.
# In a campaign the agent never exits — the host stops each rollout at its
# deadline Moment.

BB=/bin/busybox

$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB mount -t tmpfs tmpfs /tmp
$BB chmod 1777 /tmp
$BB chmod 0666 /dev/console

# The pre-launch marker the box harness can drive boot to before handing the
# run to the campaign (the base seal itself is the agent's setup_complete
# SnapshotPoint, after the start tile's X/Y registers are published). The
# agent prints its MAZE_SPEC line at startup — the manifest cross-check the
# offline report reads off the serial.
echo "MAZE_READY: launching maze-agent"
/opt/harmony/maze-agent
rc=$?
echo "MAZE_EXIT: maze-agent exited rc=$rc"

if [ "$rc" != "0" ]; then
    echo "MAZE_CRASH_TERMINAL: reboot (maze-agent failed)"
    exec $BB reboot -f
fi
echo "MAZE_CLEAN_TERMINAL: halt"
exec $BB halt -f
