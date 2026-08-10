#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Stage the one-shot boot into the self-built stock linux-6.18.35 (task 122,
# AA-1 measurement host; the AA-3 patched build boots the same way later).
# SAFETY POSTURE — an unbootable kernel must self-recover on a remote box with
# no console:
#   1. GRUB_DEFAULT=saved, with the saved default pinned to the KNOWN-GOOD
#      delivered kernel (6.8.0-134) — a power cycle always returns to it.
#   2. `grub-reboot` arms 6.18.35 for the NEXT BOOT ONLY.
#   3. `panic=30` on the cmdline: a panicking kernel reboots after 30s, and the
#      one-shot entry has been consumed, so the box comes back on 6.8.0-134.
#
# Run with sudo. Does NOT reboot — the reboot is a separately authorized step.
set -euo pipefail

GOOD="Advanced options for Ubuntu>Ubuntu, with Linux 6.8.0-134-generic"
TRIAL="Advanced options for Ubuntu>Ubuntu, with Linux 6.18.35"

sed -i 's/^GRUB_DEFAULT=.*/GRUB_DEFAULT=saved/' /etc/default/grub
if ! grep -q "panic=30" /etc/default/grub; then
  sed -i 's/^GRUB_CMDLINE_LINUX="/GRUB_CMDLINE_LINUX="panic=30 /' /etc/default/grub
fi
grep -E '^GRUB_DEFAULT|^GRUB_CMDLINE_LINUX=' /etc/default/grub

update-grub

# Both entries must exist before either default is pointed at them.
grep -q "Ubuntu, with Linux 6.8.0-134-generic" /boot/grub/grub.cfg
grep -q "Ubuntu, with Linux 6.18.35" /boot/grub/grub.cfg

grub-set-default "$GOOD"
grub-reboot "$TRIAL"
grub-editenv list

echo "STAGED: next boot = 6.18.35 (one-shot); default/fallback = 6.8.0-134"
