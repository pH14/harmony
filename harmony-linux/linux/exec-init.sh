#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# /init of the **exec-capable** guest image (task 81). Unlike the minimal
# `init.sh` (which announces readiness and powers off) or the Postgres workload
# image (which drives postgres to a terminal), this image hands the serial console
# to an interactive **root shell** and keeps it running — so the `exec`
# improvisation channel has a shell reading ttyS0 to inject commands into.
#
# `exec` (control-proto `Request::Exec`) injects bytes on the guest's 8250 serial
# input (RBR) as if typed at this shell, and detects completion by watching the
# serial OUTPUT for a sentinel `echo` (see consonance/vmm-core/src/exec.rs). For
# that to work the shell must (a) READ ttyS0, (b) ECHO what it reads, and (c) run
# the injected `echo`. An interactive `sh -i` on the console with a controlling tty
# (via `setsid -c`) does all three: the tty line discipline echoes input in cooked
# mode, and the shell runs each line.
#
# This is a **deliberately crude, off-record** transport — `exec` taints its
# timeline by ruling (docs/RESOLUTION.md §Improvisations), so nothing here needs to
# be deterministic. The image exists purely to give gate 2 a shell to talk to; the
# taint guard (gate 3) and gate 2's determinism half hold against ANY image.
set -e
/bin/busybox mount -t proc proc /proc
/bin/busybox mount -t sysfs sysfs /sys
/bin/busybox mount -t devtmpfs dev /dev 2>/dev/null || true
# A stable, quiet prompt keeps the serial capture predictable for the sentinel
# scanner (which ignores everything but the marker line anyway).
export PS1='# '
echo GUEST_READY
# Hand the console to an interactive root shell, in a fresh session with a
# controlling tty so input is echoed and job control works. Loop so an `exec` that
# happens to exit the shell (e.g. injects `exit`) gets a fresh one rather than
# panicking init. `poweroff` is never reached — the VMM tears the fork down.
while true; do
    /bin/busybox setsid -c /bin/sh -i </dev/console >/dev/console 2>&1 || true
done
