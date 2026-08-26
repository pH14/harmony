#!/bin/bash
exec taskset -c 3 qemu-system-x86_64 \
  -enable-kvm -cpu host,pmu=on -smp 1 -m 8192 \
  -kernel /boot/vmlinuz-6.18.35+ \
  -initrd /boot/initrd.img-6.18.35+ \
  -append "root=/dev/vda rw console=ttyS0,115200 net.ifnames=0 nokaslr" \
  -drive file=/root/l1.img,format=raw,if=virtio \
  -virtfs local,path=/root/l1share,mount_tag=share,security_model=none,id=share \
  -nographic
