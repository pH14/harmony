# Task 114 — SIGSTOP-cycling wedge (hm-440)

Diagnoses the nested-x86 spike's N-3 SIGSTOP/SIGCONT wedge, finds its shipping-crate
analogue, and turns the one **silent hang** it exposes into a **loud, typed refusal**.
Surface: `consonance/vtime` (the planner) + `consonance/vmm-backend` (the `run_until`
seam that drives it). No other crate needed a change; `consonance/vmm-core` already
propagates the new error out of its run loop via `?`.

**Bottom line:** the fix is a fail-closed step budget on the single-step walk inside
`InjectionPlanner::stop_at`. It never fires in normal operation and is byte-for-byte
behaviour-preserving on every reaching path; it only converts "step forever" into a
`VtimeError::StepBudgetExceeded`. Never a silent hang.

## Diagnosis (mine the spike evidence → the shipping seam)

**Spike evidence** (`spike/nested-x86`, `spikes/nested-x86/results/n3/pause-sigstop-001/`):

- `FINDING.json` — config: `SIGSTOP 2s of every 7s` against the L1 QEMU while the
  repeat gate ran nested. Observed: "gate produced no output past rep <100 for ~27min;
  207+ pauses delivered; after pauser stopped + SIGCONT, gate did NOT recover; **vCPU
  thread spinning in KVM_RUN at ~72% (wchan=0/R)**, main loop in poll." Interpretation:
  "a nested work-clock event (**vPMU overflow PMI or MTF completion) was most plausibly
  lost across the process freeze; `run_until` then waits indefinitely** — an observable
  hang."
- `stuck-diagnostics.txt` confirms the shape: one thread at `72.7% Rl` (running in
  `KVM_RUN`), the iothread in `futex_wait`, the main loop in `do_sys_poll`.
- The condition is **rare**: the accepted-evidence cadence (2s/30s) and the recert
  (417/417 confirmed pauses, 2026-07-14) never wedged; only the aggressive 2s/7s cadence
  did, once.

**What actually wedged.** The `AUDIT-2026-07-12.md` names the mechanism: `run_until` is a
**shipping `vmm-core`/`vmm-backend` function**, and the gate ran it *nested* inside the
frozen L1. So the "vCPU thread spinning in KVM_RUN" is harmony's own `vmm-core` vCPU
thread driving the `run_until` preemption path (task 47/55): it arms a retired-branch
work-clock deadline and single-steps to land on it exactly. When a work-clock completion
is lost across the process freeze, the guest makes no further counted-event progress and
`run_until` never returns.

**Thread/lock chain (shipping code).** `Vmm::step`
(`consonance/vmm-core/src/vmm.rs:1504`) calls `self.backend.run_until(deadline)?`. On the
real backend that lands in `KvmBackend::run_until` → `drive_run_until`
(`consonance/vmm-backend/src/run_until.rs`) → `vtime::InjectionPlanner::stop_at`
(`consonance/vtime/src/planner.rs`). `stop_at`'s Phase 2 is:

```rust
while current < target {
    current = backend.single_step()?;   // advances work by 0 or 1
    single_steps_used += 1;
}
```

This loop is **unbounded**. The module doc admitted it verbatim: *"A guest that never
retires another counted event would step forever — exactly as on real hardware, where
such a deadline work count is simply never reached."* That is a **silent hang** — no
error, no bound — which the bead forbids: a determinism substrate must survive suspension
or refuse it loudly. Under SIGSTOP-cycling, a lost MTF/overflow completion is exactly the
condition that leaves the guest making no counted-event progress, so the walk spins
forever (the vCPU thread pinned in `KVM_RUN`, single-stepping and never reaching the
target). No lock is *held* — it is a livelock on the work counter, which is why nothing
else notices.

## Fix — fail closed with a deterministic step budget

The counted-event distance Phase 2 must cover is at most `skid_margin`, so at most
`skid_margin` steps ever make progress; the *only* source of unboundedness is a run of
**consecutive no-progress steps** (a guest retiring no counted event). We bound exactly
that:

- **`vtime`**: new `PlannerConfig.max_stall_steps` — the max consecutive single-steps
  with no work progress before failing closed. `stop_at`'s Phase 2 tracks a `stall`
  counter that **resets on every step that advances work** and trips only when it exceeds
  the budget, returning the new `VtimeError::StepBudgetExceeded { target, last_work,
  stall_steps }`. Because it resets on progress, it never trips on a merely *sparse* (but
  progressing) stream — only on a genuine stall. The loop is now **provably terminating**
  (total steps ≤ `skid_margin · (max_stall_steps + 1)`).
- **`vmm-backend`**: `drive_run_until` maps `StepBudgetExceeded` to a loud, self-
  describing `BackendError::Internal` (alongside the existing `SkidExceeded` arm). The
  production budget is `run_until::STALL_STEP_BUDGET = 1 << 24` (~16.7M): far above the
  longest branch-free run any certified workload (the insn gates, Postgres) shows between
  two counted events near a deadline — so it never fires in normal operation (a
  compile-time `const _` asserts `SKID_MARGIN < STALL_STEP_BUDGET < u64::MAX`) — yet
  finite, so a real wedge fails loud in bounded time (~seconds of single-stepping)
  instead of hanging forever. `Vmm::step`'s `?` then propagates it out of the run loop:
  the hang becomes a returned error.

**Behaviour-preserving on the reaching path.** For any backend that reaches the target,
`current`/`stopped_at`/`single_steps_used` and the returned `Exit` are byte-identical to
before (the `stall` bookkeeping only adds a monotonic guard). Existing planner/`run_until`
tests set `max_stall_steps: u64::MAX` (backstop disabled) so their outcomes are unchanged;
only the box `KvmBackend` and the new tests use a finite budget.

## Regression tests (portable, mock-driven — the SIGSTOP-cycle repro surface)

The mock stands in for what SIGSTOP-cycling does to the run loop: a guest that, post-
freeze, retires no further counted event (a lost work-clock completion).

- `vtime` `planner::tests`:
  - `permanent_stall_fails_closed_instead_of_hanging` — a backend that single-steps
    forever without advancing work → `StepBudgetExceeded`, and asserts the walk stopped
    after exactly `budget + 1` steps (**bounded**, not hung).
  - `stall_after_overflow_fails_closed` — same wedge reached *through Phase 1* (overflow
    lands short, then the single-step walk stalls: the MTF-completion-lost shape).
  - `sparse_but_progressing_stream_does_not_trip` — a stream whose no-progress runs come
    right up to the budget still reaches the target (proves the reset-on-progress
    semantics: no false-positive refusals).
- `vmm-backend` `run_until::tests`:
  - `stalled_guest_fails_closed_not_hung` — a stalling `PreemptCpu` through
    `drive_run_until` yields a loud, self-describing `BackendError::Internal`, in bounded
    step count.

## Spike-only vs shipping disposition

- **Shipping exposure: EXPOSED, now fixed.** The wedge's fixable form lives in shipping
  crates (`vtime::InjectionPlanner::stop_at`'s single-step walk, driven by
  `vmm-backend::drive_run_until`). It is **pure logic** — `run_until.rs` issues no
  syscall and is unit/property-tested against `vtime::sim::SimCpu` on macOS — so both the
  repro and the fix are portable. This is where the fix lands.
- **The QEMU L1 wedge is not our code.** In the spike, the *outer* L1 VMM is QEMU; the
  spike harness (`harness/run-n3-pause.sh`) already parameterizes + records the cadence
  and counts only confirmed pauses (PR #98 recert). No spike-harness change is warranted.
- **Residual (box-only, out of scope): the Phase-1 blocked-`KVM_RUN` variant.** If the
  *overflow itself* is lost, `run_until_overflow` (a single blocking `ioctl(KVM_RUN)` in
  `consonance/vmm-backend/src/kvm_sys.rs`, `#[cfg(target_os = "linux")]`) never returns —
  a blocked syscall, not a pure-logic loop. A step budget cannot interrupt a blocked
  ioctl; that needs an ioctl-level watchdog / `KVM_RUN` timeout, which is box-bound and
  cannot be portably reproduced or tested. The FINDING is genuinely ambiguous between the
  two ("PMI **or** MTF completion"); this change closes the pure-logic (single-step) half
  loudly and names the ioctl half explicitly. **Suggested follow-up:** a `KVM_RUN`
  suspension watchdog on the box backend (file as a P3 if pursued).

## Deviations considered and rejected

- **A wall-clock timeout on the wait.** Rejected: non-deterministic (forbidden by rule 4)
  and it would make the deadline outcome host-timing-dependent. The budget is a pure
  function of the instruction stream, so same seed → same trip-or-not decision.
- **Bounding total steps rather than consecutive no-progress steps.** Rejected: a total
  bound risks false-positiving a legitimately sparse-but-progressing stream. Resetting on
  every counted event bounds only a genuine stall, never sparsity.
- **A module `const` instead of a `PlannerConfig` field.** Rejected: a config field keeps
  the backstop first-class/tunable and lets the regression tests trip it with a tiny
  budget (a handful of steps) instead of looping ~16.7M times — fast, deterministic tests.
- **Delivering the imprecise stop instead of erroring.** Rejected: it would inject at a
  non-exact work count — a determinism violation. Fail-closed is the contract.

## Known limitations / integrator notes

- `PlannerConfig` gains a **required** field (`max_stall_steps`); all in-tree
  constructors (both crates, all in this surface) are updated. `vtime`'s frozen public-
  API snapshot (`consonance/vtime/tests/public-api.txt`) is updated for the new field and
  the new `StepBudgetExceeded` variant.
- The production budget (`STALL_STEP_BUDGET = 1 << 24`) is a **liveness backstop, not a
  perf knob**: it is not meant to fire in normal operation. If a future workload
  legitimately single-steps through a >16.7M-instruction branch-free region at a deadline
  (it would already be pathologically slow, and none of the certified workloads do), it
  should be raised — the value is documented at its definition with that reasoning.
- No `unsafe` touched, so Miri was not required by the task gate; the new `run_until`
  test is Miri-compatible (pure logic) and runs under the existing `cases()`-gated suite.

## Gates (all green, laptop)

- `cargo build --workspace --all-features` — ok
- `cargo nextest run --workspace --all-features` — **1696 passed, 29 skipped**
- `cargo clippy -p vtime -p vmm-backend --all-features --all-targets -- -D warnings` — ok
  (the residual `rand::thread_rng`/`rand::random` warnings are pre-existing repo-wide
  `clippy.toml` config notes, not lint denials)
- `cargo fmt -p vtime -p vmm-backend -- --check` — ok
- `cargo deny check` — advisories/bans/licenses/sources ok
