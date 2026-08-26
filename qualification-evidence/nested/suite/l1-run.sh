#!/bin/sh
O=/share/out-suite
rm -rf $O && mkdir -p $O
modprobe msr 2>/dev/null
# The kernel refuses to raise the sample rate while the throttle is off, so the
# rate goes first and the throttle second.
sysctl -w kernel.perf_event_max_sample_rate=100000000
sysctl -w kernel.perf_cpu_time_max_percent=0
echo "cpus: $(nproc)  cmdline: $(cat /proc/cmdline)"
echo "=== check ==="
taskset -c 1 /share/cpu-qualification check --baseline det-zen3-v1 \
  --dispositions /share/guest-dispositions.toml > $O/check.txt 2>&1
echo "check rc=$?"
grep -E "DEVIATION|deviation \(|every required" $O/check.txt
echo "=== run --stage 1 ==="
taskset -c 1 /share/cpu-qualification run --stage 1 --baseline det-zen3-v1 \
  --evidence-dir $O/run --dispositions /share/guest-dispositions.toml > $O/run.txt 2>&1
echo "run rc=$?"
tail -6 $O/run.txt
echo "=== report ==="
/share/cpu-qualification report --evidence-dir $O/run > $O/report.json 2>&1
echo "report rc=$?"
cat $O/report.json
echo "=== done ==="
