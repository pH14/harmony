#!/bin/sh
# Runs at L1 boot. Mounts the share the host passes in, runs whatever script it
# holds, leaves the output there, powers off. Keeps the guest reachable without
# a login.
exec >/dev/console 2>&1
echo "=== L1 runner starting ==="
modprobe 9pnet_virtio 2>/dev/null
modprobe 9p 2>/dev/null
mkdir -p /share
if mount -t 9p -o trans=virtio,version=9p2000.L share /share; then
    echo "share mounted"
    if [ -x /share/l1-run.sh ]; then
        /share/l1-run.sh > /share/l1-out.txt 2>&1
        echo "--- l1-out.txt ---"
        cat /share/l1-out.txt
    else
        echo "no executable /share/l1-run.sh"
    fi
    sync
    umount /share
else
    echo "share mount FAILED"
fi
echo "=== L1 runner done, powering off ==="
systemctl poweroff
