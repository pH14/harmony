# Task 56 — SMP-cpuset k3s bring-up: get Postgres-on-k3s deterministic (frontier)

> **Frontier / box-heavy. CONTINUE the live bring-up on the box at `/root/ht49`** — do NOT
> start from scratch. The SMP guest kernel and the MTF (patch 0005) KVM module are already
> built there and the repo changes are applied (uncommitted). Your job: get k3s (then
> Postgres) running **deterministically** on the SMP kernel, starting by fixing a terminal
> idle-HLT. You work on the box; the foreman steers you and reviews.

## Context — what's already done (all on the box `/root/ht49`)
The #34 deterministic-preemption bug is FIXED. Patch **0005** (MTF single-step:
`KVM_ARM_MTF_STEP` ioctl + `handle_monitor_trap` → `KVM_EXIT_DET_STEP`; userspace
`single_step_once` arms MTF each step) eliminated the Phase-2 single-step overshoot
(box-proven: `live_k3s_postgres` passed the old step-155932 `SkidExceeded`, `step_error=None`).
k3s (`live_k3s_postgres`, gate `k1_k3s_cluster_postgres_client_streams_patched`) was then
pushed past three guest-env layers:
- **networking** — `guest/linux/k3s-init.sh`: default route via `lo` + `--node-ip=10.42.0.2`
- **cgroups** — added `cpuset` to the subtree_control loop
- **cpuset needs SMP** — rebuilt the guest kernel `CONFIG_SMP=y` + `maxcpus=1` (dropped
  `nosmp`); held the determinism levers down (CPU_FREQ stays off via
  `# CONFIG_SCHED_MC_PRIO is not set`, which otherwise selects the cpufreq stack). All
  determinism build-asserts pass.

Uncommitted box changes: `guest/linux/k3s-init.sh`, `guest/linux/config-fragment`,
`guest/linux/build-kernel.sh`, `consonance/vmm-core/tests/live_k3s_postgres.rs` (cmdline),
`consonance/vmm-backend/src/kvm_sys.rs` (MTF). The **0005 kernel patch** lives in the deb612
build tree (`/root/kvm-spike/deb612/hdr/.../linux-headers-6.12.90+deb13.1-{amd64,common}/`);
the built module is `$B/kvm.ko` (loaded by `/root/run-patched-ht49.sh`). The SMP bzImage is
`guest/build/bzImage`. Decision log: `~/workspace/harmony-autonomous-decisions.md` (read the
2026-06-29/30 entries). Run with: `/root/run-patched-ht49.sh <timeout> cargo test -p vmm-core
--test live_k3s_postgres -- --ignored --nocapture --test-threads=1
k1_k3s_cluster_postgres_client_streams_patched`.

## STEP 0 — preserve the box state (first thing)
On the box in `/root/ht49`: commit the uncommitted repo changes to a branch
`task/smp-cpuset-k3s-bringup` (preserve + track). Capture the 0005 kernel diff as a patch
file (diff the deb612 headers tree against stock, or reconstruct) and note its location. Do
NOT push yet — tell the foreman when it's committed.

## The immediate blocker — terminal idle-HLT (diagnose, then fix)
The SMP k3s run terminates at **step 98078, `terminal=Hlt`, `step_error=None`** — right after
`Run /k3s-init`, BEFORE k3s starts. The UP kernel never did this (tiny-RCU never idle-waits;
SMP tree-RCU does). The single-vCPU HLT-resume model (task 52 — find it in the vmm run loop /
`consonance/vmm-backend`) treats this idle HLT as terminal instead of resuming it.
1. **Diagnose:** instrument the point where `Exit::Hlt` is classified terminal; log at the
   terminal HLT: guest `RFLAGS.IF`, whether a LAPIC timer is armed, RIP, work/step count. Run
   the gate, read the DIAG line.
2. **Fix:** teach HLT-resume to **deterministically** resume the SMP idle wait (e.g. IF=1 ⇒
   advance V-time to the next armed timer / arm one + inject the tick; only IF=0 with nothing
   pending is genuinely terminal). The resume must be a pure deterministic function of guest
   state — re-run and confirm `step_error=None` and no `skid exceeded`/`DIAG-SKID49`.

## Then — push k3s + validate determinism
Re-run the gate; peel each next layer (kubelet/containerd/CNI → pods → Postgres) — each
failure so far has been ordinary guest-env config, NOT the determinism engine. **North star:**
k3s cluster Ready → Postgres server pod + client pod (intra-guest CNI) → `GUEST_READY`,
**deterministic-twice** (bit-identical `state_hash` / the gate's r2). Every run: confirm NO
`skid exceeded` / `DIAG-SKID49` (that would be a determinism regression — escalate
immediately) and that the run is deterministic.

## Box-safety (CRITICAL — non-negotiable)
- Stock KVM = **1396736**; the 0005 module is larger. ALWAYS leave the box on stock 1396736
  when you stop. Revert: `pkill -9 -f live_k3s_postgres` FIRST → wait `lsmod|grep '^kvm_intel'`
  users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` → verify size 1396736 on a
  FRESH ssh connection. run-patched's own revert is unreliable — always verify + manually revert.
- SSH drops (exit 255) on pkill/rmmod are normal — reconnect + verify.
- Pin builds/tests to `taskset -c 2` (docs/BOX-PINNING.md).

## Working discipline
- Work ON THE BOX in `/root/ht49` (the built artifacts are there — don't rebuild from scratch).
- Drive builds/tests in the FOREGROUND in your turn (or background + poll yourself); READ the
  result before reporting. Do NOT set up a detached poller and go idle.
- Report progress + the determinism status every turn. Escalate to the foreman if: a skid /
  determinism regression appears, the HLT fix turns out fundamental (not a small resume tweak),
  or you're blocked.

## Non-goals (for now)
- Productionizing 0005 (clean patch + canonical linux-6.18 port + full gates) — separate, later.
- Optimizing the SMP slowness (~1.7×/step) — note it; correctness first.
