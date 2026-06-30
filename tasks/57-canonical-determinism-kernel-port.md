# Task 57 — Productionize the determinism kernel: canonical linux-6.18.35 port of patches 0004 + 0005

> **Box-heavy, determinism-core. Make the determinism kernel work LEGIT.** The frontier
> (Postgres-on-k3s deterministic-twice, task 49/56) was proven on the box-proxy **6.12.90**
> build. The pinned canonical gate kernel is **linux-6.18.35**. Port patches **0004**
> (in-kernel force-exit, Phase-1) **+ 0005** (MTF single-step, Phase-2) onto the canonical
> tree, build, and re-validate the determinism on it. The foreman steers + reviews.

## Context (what exists)
- **0005 (MTF single-step)** — the #34 Phase-2 fix — is a clean repo patch:
  `consonance/vmm-backend/kvm-patches/patches/0005-*` (patch + NOTES), validated on the
  6.12.90 deb612 box-proxy build. It adds `KVM_ARM_MTF_STEP` ioctl + `handle_monitor_trap`
  → `KVM_EXIT_DET_STEP`, arming `CPU_BASED_MONITOR_TRAP_FLAG`. Userspace integration
  (`kvm_sys.rs single_step_once`, `run_until`, `vmm` HLT-resume) is on branch
  `task/smp-cpuset-k3s-bringup` (tag `task56-k2-pass`, on origin).
- **0004 (force-exit, Phase-1)** is the task-55 work (PR #33 / the patch series). For the
  canonical kernel, BOTH 0004 + 0005 are needed (Phase-1 + Phase-2 = full determinism).
- Patches 0001–0003 (RDTSC/RDRAND/RDSEED exiting + emulation) already exist in the series.
- Canonical kernel source is on the box: `/root/kvm-spike/linux-6.18.35`. The box-proxy
  (6.12.90) tree the frontier validated on: `/root/kvm-spike/deb612/`. Read
  `docs/CPU-MSR-CONTRACT.md` (the patched-KVM patch series + versions.lock),
  `tasks/55-deterministic-force-exit-preemption.md`, and the branch's
  `IMPLEMENTATION-task56.md` (HANDOFF section — exact patch locations).

## Task
1. **Port + build.** Apply patches 0001→0005 onto pinned `linux-6.18.35` (`git am` or the
   series' apply harness). The 6.18 `vmx.c`/`x86.c` differ from 6.12 — adapt 0004/0005's
   hunks as needed (same mechanism: `handle_monitor_trap`/`handle_exception_nmi` hooks, the
   arm ioctls, the new exit reasons, the per-vcpu bools). Build `kvm.ko`/`kvm-intel.ko`
   reproducibly; document in `BUILD.md`.
2. **Validate determinism on the canonical kernel.** Load the canonical-built module and run,
   in order of cost: (a) host-assert + M1/M2 determinism gates, (b) `live_runc_postgres`
   deterministic-twice (faster), (c) the k3s gate `k1` then — budget permitting — `k2`
   (deterministic-twice). **Pass criteria: bit-identical `state_hash`, `step_error=None`,
   ZERO `skid exceeded`/`DIAG-SKID49`.** This proves the frontier determinism holds on the
   pinned kernel, not just the box-proxy.
3. **Series + lockfiles.** Add 0005 to the canonical patch series; update
   `docs/CPU-MSR-CONTRACT.md` (now 5 patches; the `KVM_ARM_MTF_STEP`/`KVM_EXIT_DET_STEP`
   ABI), `guest/linux/versions.lock` / any patch-hash manifest. Keep the box-proxy build
   reproducible too.

## Box-safety (CRITICAL)
Stock KVM = **1396736**; ALWAYS leave the box stock + verified after every run
(`pkill -9 -f live_*`; wait users=0; `rmmod kvm_intel kvm; modprobe kvm kvm_intel`; verify
size on a FRESH ssh). SSH drops on pkill/rmmod are normal — reconnect + verify. Pin to
`taskset -c 2`. Run gates in the foreground + READ results before reporting; no detached
pollers + idle.

## Working discipline
- Base off `task/smp-cpuset-k3s-bringup` (has 0005 + the userspace integration). Work on the
  box where the kernel trees + build harness live.
- Report progress + the determinism status each turn. ESCALATE to the foreman if: a
  determinism regression appears on the canonical kernel (skid / non-bit-identical), the
  0004/0005 hunks don't port cleanly to 6.18 and need a design change, or you're blocked.

## Non-goals (separate, foreman-driven)
- Splitting the task-56 bundle into reviewable PRs + the cross-model review (foreman).
- The SMP-slowness perf optimization.
- Re-architecting the patches — port the proven mechanism, don't redesign.
