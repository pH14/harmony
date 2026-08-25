# Booting the patched kernel: what the staging discipline could and could not guarantee

## The initrd guard returned a false negative

`stage-6.18-boot.sh install` refused to stage, reporting that the 6.18.35 initramfs
was missing `md/raid1.ko` and `nvme/host/nvme.ko`. Both modules are present. The
check is

    if ! sudo lsinitramfs "/boot/initrd.img-$KVER" | grep -q "$m"; then ...

under `set -euo pipefail`. `grep -q` exits at the first match and closes the pipe,
`lsinitramfs` takes SIGPIPE, and `pipefail` makes the pipeline's status 141, so a
pattern that matches *early* is reported as missing. Demonstrated on the box:

    set -o pipefail; lsinitramfs /boot/initrd.img-6.18.35 | grep -q "md/raid1.ko"
    status=141                       # same command without pipefail: found

The initramfs was then checked by hand and carries
`drivers/md/raid1.ko.xz` and `drivers/nvme/host/nvme.ko.xz`. Staging proceeded
through the script's `stage` step. The defect is in the spike tree
(`spike/amd-epyc:spikes/amd-epyc/host/stage-6.18-boot.sh`), not in this repository.

## The one-shot boot is not self-recovering on this box

`grub-reboot` warned: "Detected GRUB environment block on diskfilter device". `/boot`
is md0, a RAID1, and GRUB cannot write `grubenv` from within GRUB on a diskfilter
device. It reads `next_entry` and boots the patched kernel, but the `save_env` that
would clear the one-shot silently fails, so `next_entry` persists across boots. The
designed recovery — one boot into the new kernel, automatic revert to the stock
kernel on any failure — does not hold here. A kernel that failed to reach userspace
would have boot-looped, and this box has no out-of-band console available to this
program.

`GRUB_CMDLINE_LINUX_DEFAULT` was trimmed to `panic=30` rather than left as the
staging step writes it. The box already carries `console=tty0 console=ttyS0,115200`
in `GRUB_CMDLINE_LINUX` at the baud its serial console runs at; the staging step's
`console=tty1 console=ttyS0` would have appended a second, baud-less serial console
and taken over `/dev/console`.

## What was checked before rebooting

The patched kernel and its initramfs were booted inside a KVM guest on the box first
(`qualification-evidence/box/stage2/qemu-preflight.log`):

    qemu-system-x86_64 -enable-kvm -cpu host -m 2G -smp 2 -nographic -no-reboot \
      -kernel /boot/vmlinuz-6.18.35 -initrd /boot/initrd.img-6.18.35 \
      -append "console=ttyS0 panic=5 rdinit=/bin/sh -- -c :"

It reached userspace: `Linux version 6.18.35 ... #1 SMP PREEMPT_DYNAMIC`, the
initramfs unpacked, `/bin/sh` ran and exited, and the kernel panicked on init exiting
as that command line asks it to. The kernel boots on this silicon and its initramfs
works. It does not prove the md1 root mounts, which the guest has no way to exercise;
that rests on the initramfs carrying the root stack and on the configuration being
`olddefconfig` from the config the box's own working kernel booted with.
