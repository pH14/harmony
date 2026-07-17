#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# stage-6.18-boot.sh — AE-3 (Paul ruling 2026-07-17): install the patched 6.18.35
# .deb and boot into it with a SELF-RECOVERING GRUB one-shot, mirroring the ARM
# spike's boot-staging discipline. This is the FIRST live boot of the x86 6.18.35
# determinism kernel (task 57 built+verified it but never booted it), so the boot
# is fully guarded:
#
#   - one-shot `grub-reboot` into 6.18.35: GRUB boots it EXACTLY ONCE, then the next
#     boot reverts to the permanent saved-default (the running stock 6.8). A hang or
#     a failure-to-consume never strands the box on the new kernel.
#   - saved-default pinned to the running 6.8 kernel (record-then-modify: the exact
#     restore target), so recovery is automatic.
#   - panic=30 in the cmdline: a kernel panic (e.g. root not mountable) auto-reboots
#     after 30s -> GRUB boots the saved 6.8 (the one-shot already consumed).
#   - GRUB_RECORDFAIL_TIMEOUT=5: a failed boot shows the menu for 5s and proceeds to
#     the saved default rather than waiting forever for input on a headless box.
#
# Boot-safety of the kernel itself is in build-6.18-kernel.sh (config based on the
# running Ubuntu config so md1 RAID1 root + NVMe drivers are present) plus a
# MODULES=dep initramfs (set here) probed against THIS box's real root stack.
set -euo pipefail
SD=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
KVER=6.18.35
WORK="${KBUILD_WORK:-$HOME/kbuild-618}"
OUT="${AE3_OUT:-$HOME/amd-epyc-spike/results/ae-3}"
RUNREL=$(uname -r)
GRUB=/boot/grub/grub.cfg
mkdir -p "$OUT"
log(){ echo "[stage-6.18] $*" >&2; }

# menuentry_id for the entry whose title contains $1 (excluding recovery), from grub.cfg
entry_id_for() {
  sudo grep -E "menuentry " "$GRUB" | grep -F "$1" | grep -v recovery | \
    grep -oP "\\\$menuentry_id_option '\K[^']+" | head -1
}
submenu_id() {
  sudo grep -E "submenu " "$GRUB" | grep -oP "\\\$menuentry_id_option '\K[^']+" | head -1
}

install() {
  local deb hdr want have
  deb=$(ls "$WORK"/linux-image-"$KVER"_*.deb | head -1)
  [ -f "$deb" ] || { log "no linux-image .deb in $WORK — build first"; exit 2; }
  # Content-hash gate (evidence integrity #3): verify the .deb against the pin the
  # build recorded, BEFORE installing. A hash mismatch is a refusal, not a warning.
  want=$(python3 -c 'import json;print(json.load(open("'"$OUT"'/patched-kernel.json"))["image_deb_sha256"])')
  have=$(sha256sum "$deb" | awk '{print $1}')
  [ "$want" = "$have" ] || { log "IMAGE .deb HASH MISMATCH ($have != $want) — refusing install"; exit 3; }
  log "linux-image .deb sha256 verified against the build pin"
  # Small initramfs (fits the tight /boot) that still carries THIS box's root stack.
  echo 'MODULES=dep' | sudo tee /etc/initramfs-tools/conf.d/amd-spike-dep.conf >/dev/null
  sudo dpkg -i "$deb"
  hdr=$(ls "$WORK"/linux-headers-"$KVER"_*.deb 2>/dev/null | head -1)
  [ -n "$hdr" ] && sudo dpkg -i "$hdr" || true
  # Post-install verification: the new kernel + initrd exist and the initrd actually
  # carries the RAID1 + NVMe modules this box mounts root from (else it can't boot).
  [ -f "/boot/vmlinuz-$KVER" ] || { log "vmlinuz-$KVER missing after install"; exit 4; }
  [ -f "/boot/initrd.img-$KVER" ] || { log "initrd.img-$KVER missing after install"; exit 4; }
  local miss=0
  for m in raid1 md_mod nvme; do
    if ! sudo lsinitramfs "/boot/initrd.img-$KVER" | grep -q "$m"; then
      log "WARNING: initrd for $KVER is missing module '$m' (root is on md1 RAID1/NVMe)"; miss=1
    fi
  done
  [ "$miss" = 0 ] && log "initrd carries raid1 + md_mod + nvme (root stack present)" || \
    { log "STOP: initrd lacks the root stack — boot would fail; not staging"; exit 4; }
  sudo update-grub
  log "installed $KVER; vmlinuz+initrd present, root modules verified, grub updated"
}

stage() {
  # record-then-modify: snapshot the grub baseline (the restore inputs).
  sudo cp /etc/default/grub "$OUT/default-grub.baseline"
  sudo grub-editenv /boot/grub/grubenv list > "$OUT/grubenv.baseline" 2>/dev/null || true

  # GRUB_DEFAULT=saved + a small recordfail timeout + panic=30 (global; harmless on
  # 6.8). Keep the serial console verbose (drop quiet/splash) so a bad boot is
  # diagnosable over ttyS0.
  sudo sed -i \
    -e 's/^GRUB_DEFAULT=.*/GRUB_DEFAULT=saved/' \
    -e 's/^GRUB_CMDLINE_LINUX_DEFAULT=.*/GRUB_CMDLINE_LINUX_DEFAULT="console=tty1 console=ttyS0 panic=30"/' \
    /etc/default/grub
  grep -q '^GRUB_RECORDFAIL_TIMEOUT=' /etc/default/grub \
    || echo 'GRUB_RECORDFAIL_TIMEOUT=5' | sudo tee -a /etc/default/grub >/dev/null
  sudo update-grub

  local run_id new_id sub
  run_id=$(entry_id_for "$RUNREL")            # the stock 6.8 top-level entry
  new_id=$(entry_id_for "$KVER")              # the 6.18.35 entry (under Advanced)
  sub=$(submenu_id)
  [ -n "$run_id" ] || { log "cannot find grub entry for running $RUNREL"; exit 5; }
  [ -n "$new_id" ] || { log "cannot find grub entry for $KVER"; exit 5; }

  # Permanent fallback = the stock 6.8 kernel. One-shot next boot = 6.18.35.
  sudo grub-set-default "$run_id"
  if [ -n "$sub" ]; then sudo grub-reboot "${sub}>${new_id}"; else sudo grub-reboot "$new_id"; fi

  { echo "saved_default (permanent fallback): $run_id"
    echo "one-shot next_entry (this boot only): ${sub:+$sub>}$new_id"; } | tee "$OUT/boot-plan.txt" >&2
  log "--- grubenv after staging ---"
  sudo grub-editenv /boot/grub/grubenv list >&2
  log "STAGED. Next boot -> $KVER ONCE; any failure/hang recovers to $RUNREL. Run: $0 reboot"
}

do_reboot() { log "rebooting into $KVER (one-shot); reconnect + run: $0 verify"; sync; sudo systemctl reboot; }

verify() {
  local rel; rel=$(uname -r)
  echo "uname -r: $rel"
  [ "$rel" = "$KVER" ] || { log "NOT on $KVER (on $rel) — one-shot may have reverted; check console"; exit 6; }
  ls -l /dev/kvm
  echo "kvm_amd vermagic: $(modinfo -F vermagic kvm_amd 2>/dev/null)"
  echo "nested: $(cat /sys/module/kvm_amd/parameters/nested 2>/dev/null)"
  echo "avic:   $(cat /sys/module/kvm_amd/parameters/avic 2>/dev/null)"
  # KVM_EXIT_PREEMPT UAPI presence: the harness's cap probe returns 3 on a stock
  # kernel; here it should get past ENABLE_CAP (attests the patched mechanism).
  log "on $KVER with /dev/kvm; run the ae3 harness --smoke to attest KVM_EXIT_PREEMPT"
}

restore() {
  # Return the permanent + next boot to the stock 6.8 kernel (baseline restore).
  local run_id; run_id=$(entry_id_for "$RUNREL")
  [ -n "$run_id" ] && sudo grub-set-default "$run_id" || true
  sudo grub-editenv /boot/grub/grubenv unset next_entry 2>/dev/null || true
  log "grub default reset to $RUNREL; unset one-shot. (If currently on $KVER, reboot to return to $RUNREL.)"
}

case "${1:-}" in
  install) shift; install "$@" ;;
  stage)   stage ;;
  reboot)  do_reboot ;;
  verify)  verify ;;
  restore) restore ;;
  *) echo "usage: stage-6.18-boot.sh install|stage|reboot|verify|restore" >&2; exit 2 ;;
esac
