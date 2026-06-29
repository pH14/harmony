# Task 55 — Deterministic in-kernel force-exit preemption (patched-KVM patch 0004)

> **Determinism-core, box-only, task-47/07 class.** Replace the racy async-SIGIO overflow preemption with a
> **bounded-skid in-kernel VM-exit**, so the V-time LAPIC-timer deadline is hit deterministically for **every**
> workload — not just those whose deadlines happen to land in deterministically-deep exit-free regions. This is
> the universal-soundness fix the user chose over task 54's natural-exit fallback (which is workload-dependent).

Read `tasks/47-deterministic-preemption-timer.md` (the mechanism this upgrades), `tasks/00-CONVENTIONS.md`,
`docs/CPU-MSR-CONTRACT.md` (§ the patched-KVM patch series + versions.lock), the memory
`lapic-page-hole-unblocks-vtime`, and `~/workspace/harmony-autonomous-decisions.md` (the 2026-06-29 force-exit
design entry) first. The patched-KVM source + build harness is on the box at `/root/patched-kvm-rdtsc-run`.

## Why (the defect this fixes)

Task 47 preempts at a V-time deadline via a **userspace `perf_event`** branch counter set to overflow at
`deadline − SKID_MARGIN`; the overflow raises `O_ASYNC` `SIGIO`, whose no-`SA_RESTART` handler `EINTR`s `KVM_RUN`;
the planner then single-steps to the exact deadline. **The `SIGIO` delivery latency (kernel `irq_work`/`fasync`)
is unbounded in a CPU-bound, exit-free guest region.** A real `runc`+Postgres boot proved it: one LAPIC deadline
fell inside a **28207-branch RDTSC-to-RDTSC region**, the overflow `SIGIO` did not break `KVM_RUN` until the
trailing RDTSC, and the deterministic timer missed its injection point (`run_until` `PastDeadline`).

Task 54's natural-exit fallback delivered that timer at the next natural exit deterministically **for Postgres**
(box r2/r3 bit-identical), but cross-model review (pi) proved a residual **boundary race**: a deadline whose next
guest exit sits a knife-edge distance past it can resolve as *single-step-to-`Exit::Deadline`* **or**
*outrun-to-natural-exit* depending on the nondeterministic `SIGIO` latency → same-seed divergence (caught by the
determinism gate, but a real hole in the general guarantee). The root cause is the **unbounded** `SIGIO` skid.

## The fix

The `perf_event` overflow already fires a host **PMI** (Local-APIC LVTPC). While the guest runs (VMX non-root,
external-interrupt-exiting on), that PMI causes a **VM-exit**. KVM today runs the host PMI handler and re-enters.

**Patch 0004 (kernel):** add `KVM_CAP_X86_DETERMINISTIC_PREEMPT` and a **one-shot arm** (a `kvm_run` field or a
lightweight `vcpu` ioctl). When armed, after the perf-PMI VM-exit KVM **returns to userspace** with a new exit
reason **`KVM_EXIT_PREEMPT`** instead of re-entering. The skid is then only the **hardware PMI latency** (bounded,
~128 retired branches — well inside the `SKID_MARGIN = 256` arm-early window), **not** the unbounded `SIGIO`
delivery latency. Modeled on patch 0001's `KVM_EXIT_DETERMINISM` plumbing (exit reason + `kvm_run` payload + cap).

**Backend (userspace):** keep the `perf_event` in `pmu_sys.rs` — it still generates the overflow PMI **and** serves
the branch-counter read (the V-time anchor). Replace the `O_ASYNC`/`SIGIO` arm with KVM's one-shot exit-on-PMI arm.
In `run_until`, handle `KVM_EXIT_PREEMPT` exactly like today's `StepStop::Interrupted` (read the PMU; if
`work ≥ armed_at`, stop and single-step to the exact deadline). Because the overflow now **always** lands within
the margin, the free-run always stops strictly before the deadline → single-step → `Exit::Deadline`. **`PastDeadline`
/`AtDeadline` revert to fail-closed (loud)** — a genuine determinism violation if ever hit — and **task 54's
natural-exit fallback is removed.**

## Open implementation detail (resolve by box research first)

The hook for "KVM returns to userspace on **our** determinism counter's overflow PMI":
- **(a) preferred, lighter:** the guest PMU is off, so the only PMI source on the vCPU is our counter — when armed,
  KVM returns to userspace after the external-interrupt VM-exit whose vector is the perf LVTPC vector. No
  perf-internal coupling.
- **(b):** hook the `perf_event` overflow callback for the vCPU thread's determinism counter to set a vCPU request.

Verify (a) on the **running** kernel's `vmx.c` external-interrupt VM-exit path before committing the patch shape.
Spike it on the 6.12.90 box-proxy build (live), then port to the pinned `linux-6.18.35` (canonical gate-2 build).

## Acceptance gates

1. **Patch applies + builds, reproducibly.** `git am` patch 0004 onto pinned `linux-6.18.35` (after 0001–0003) and
   build `kvm.ko`/`kvm-intel.ko` (canonical, gate-2); also the 6.12.90 box-proxy build that actually loads. Document
   in `BUILD.md`.
2. **Box r1/r2/r3 pass with ZERO `PastDeadline`.** `live_runc_postgres` r1/r2/r3 (patched KVM **with 0004**) pass
   deterministic-twice (bit-identical serial + `state_hash`) **and** the run logs show **no** `PastDeadline`/overshoot
   — the proof the in-kernel skid is bounded within the margin. (Contrast: task-54 box runs showed exactly one
   28207-branch `PastDeadline`; with 0004 there must be none.) Always revert KVM to stock 1396736 after.
3. **Skid bound observed.** Instrument (box-local, diagnostic) the per-preemption skid (`stopped − armed_at`) across
   the boot and confirm `max skid < SKID_MARGIN`; record the observed max in `RESULTS.md`. (Drop the diagnostic before
   landing.)
4. **`run_until` fail-closed restored.** `PastDeadline`/`AtDeadline` are loud `BackendError::Internal`; the natural-exit
   `Ok(exit)` fallback is gone; portable unit/property tests assert the fail-closed disposition (the box `LiveCpu`
   never reaches it in normal operation).
5. **Contract + lockfiles.** `docs/CPU-MSR-CONTRACT.md` patch series → 4 patches; the new cap + `KVM_EXIT_PREEMPT`
   reason documented; `guest/linux/versions.lock` / any patch-hash manifest updated. Stock-revert invariant intact
   ([[box-patched-kvm-ops]]).
6. **No determinism regressions.** The existing M1/M2 host-assert + determinism gates still pass on the 0004 module.

## Coordination

- **Task 54 (PR #31, routing — worker-complete) merges WITH this** (the user's "force-exit before merge"): land 0004
  + the `run_until` fail-closed revert, then merge the combined LAPIC-V-time-tick + force-exit as the complete
  deterministic-tick milestone. PR #31's e820/memslot/region work is unchanged by this task.
- The kernel patch + box build/validation is **foreman/box** (workers can't reach the box). The userspace backend
  re-integration (`pmu_sys.rs` arm + `run_until` `KVM_EXIT_PREEMPT` handling + fail-closed revert) is portable and
  may be delegated against this spec once patch 0004's ABI is fixed.

## Non-goals

- Re-architecting the branch counter **into** KVM — keep the userspace `perf_event` for the read + PMI generation;
  0004 only adds the forced exit.
- The guest in-kernel PMU (stays off). The VMX-preemption timer (TSC-based, not branch-exact — wrong clock).
