#!/bin/bash
# Two virtual CPUs so one can be isolated inside the guest the way the baseline
# wants: guest cpu1 carries the measurement and is backed by the host's isolated
# core, guest cpu0 carries the guest's own housekeeping on an ordinary host core.
set -uo pipefail
HOST_MEASURE_CORE=3
HOST_HOUSEKEEP_CORE=5

qemu-system-x86_64 \
  -enable-kvm -cpu host,pmu=on -smp 2 -m 8192 \
  -kernel /boot/vmlinuz-6.18.35+ \
  -initrd /boot/initrd.img-6.18.35+ \
  -append "root=/dev/vda rw console=ttyS0,115200 net.ifnames=0 nokaslr isolcpus=1 nohz_full=1 rcu_nocbs=1 spec_store_bypass_disable=on" \
  -drive file=/root/l1.img,format=raw,if=virtio \
  -virtfs local,path=/root/l1share,mount_tag=share,security_model=none,id=share \
  -nographic &
QPID=$!

# Pin the vCPU threads once qemu has named them.
for _ in $(seq 1 40); do
  mapfile -t TIDS < <(grep -l "^CPU [01]/KVM$" /proc/$QPID/task/*/comm 2>/dev/null)
  [ "${#TIDS[@]}" -eq 2 ] && break
  sleep 0.25
done
for f in "${TIDS[@]:-}"; do
  [ -n "$f" ] || continue
  tid=$(basename "$(dirname "$f")")
  case "$(cat "$f")" in
    "CPU 0/KVM") taskset -pc $HOST_HOUSEKEEP_CORE "$tid" >/dev/null 2>&1
                 echo "pinned guest cpu0 (tid $tid) to host core $HOST_HOUSEKEEP_CORE" ;;
    "CPU 1/KVM") taskset -pc $HOST_MEASURE_CORE "$tid" >/dev/null 2>&1
                 echo "pinned guest cpu1 (tid $tid) to host core $HOST_MEASURE_CORE" ;;
  esac
done
wait $QPID
