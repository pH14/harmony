# SEAL-RATE-REPORT — arbitrary-V-time seal validation (task 63, the Wave-5 go/no-go)

> **Status: PROVISIONAL — measured numbers pending the box run.**
> Gate 1 (the portable sampling-schedule + seal-rate/`sealable` bookkeeping, with unit +
> proptest coverage against a mock oracle) is **complete and green** on macOS + Linux.
> Gates 2–3 require running `tests/seal_rate_sweep.rs` on the determinism box against the
> LOADED patched KVM + the built Postgres image. Per this task's `Do not push` boundary and
> the task-58 precedent (box gates run by the foreman), the definitive box measurement is
> **handed to the foreman** — see [§7 Runbook](#7-runbook-what-the-foreman-runs) and
> [§8 Handoff](#8-handoff). The numbers in [§6](#6-projected-result-model--analysis-not-a-box-measurement)
> are a **calibrated projection** from the substrate analysis in [§5](#5-substrate-analysis-the-load-bearing-reasoning),
> not a measurement; the harness prints exactly these fields, so the foreman transcribes the
> real values over the projected ones and confirms/updates the ruling in [§9](#9-the-ruling).

---

## 1. The question

The whole `docs/EXPLORATION.md` archive rests on one empirical assumption: that task 41 lets
you **seal a snapshot at an arbitrary mid-workload `Moment` and branch from it
deterministically** — not just at the handful of quiescent boundaries. Parent-rooted lazy
materialization (Phase C: store kilobyte virtual exemplars `(parent, seed', suffix)` and
materialize by `branch(parent) → run the suffix`) is **unbuildable** if the parent can only
be sealed at incidental quiescent points. This report measures whether task 41 cleared that,
and rules **GO** (Phase C proceeds unrestricted) or **NO-GO / RESTRICTED** (Phase C inherits a
`sealable(Moment)` predicate).

## 2. Baseline being beaten

Task 40 measured, under task 39's **quiescent-only** codec, **0 of 8392** post-readiness
V-time boundaries sealable on a live Postgres guest — split **5280 non-synchronized** +
**3112 in-flight injection**. Task 41 claims to have lifted the in-flight-injection limit by
capturing the full `kvm_vcpu_events` record. This task adversarially measures the *rate* at
which arbitrary V-times are now sealable, and whether a mid-workload seal branches
deterministically and survives timing perturbation.

## 3. Axis note (read this before the numbers)

Two V-time axes exist in the codebase; this measurement uses **one** of them consistently:

- The substrate's V-time clock is **retired conditional branches**, 1 ns per branch
  (`contract_vclock_config`), reported by `Vmm::effective_vns()` and consumed by a `run`
  deadline (`control-proto::VTime`). **This report samples and reports in that axis** (ns of
  V-time == retired branches). Task 63's prose says "retired-instruction count"; on this
  substrate the addressable grid is retired branches, so all figures below are V-time ns.
- `control-proto::Moment` (retired *instructions*) is a separate axis used only by
  `Perturb { at }`; it is **not** used here.

## 4. Method (what `tests/seal_rate_sweep.rs` does)

One live Postgres guest is driven forward through the real snapshot path; every measurement
is fed into the **same** `vmm_core::seal_rate` bookkeeping the portable proptest suite covers.

1. **Profiling pass** — boot to a clean terminal once (deterministic; same seed as the
   measurement passes), recording the V-time at `PG_READY` (span start), the terminal V-time
   (span end), and up to three **interrupt-service busy windows** (V-times carrying a genuine
   active event injection). (Skippable via `SPAN_START`/`SPAN_END`/`BUSY_CENTERS` for fast
   re-runs.)
2. **Sampling schedule** — `SamplingSchedule::build(span, N≥64, busy)`: `N − k` uniform points
   across the span + `k = min(busy, max(1, N/8))` points at busy-window centers, sorted
   ascending (one live guest, sampled forward).
3. **Nominal pass (§1 + §2)** — at each target: `run` to the target (lands at the first
   V-time-**synchronized** boundary at/after it), `save_vm_state` (record success / failure
   **reason**), and for each success take the memory snapshot and **prove it is a real branch
   point (§2)**: `restore + reseed_entropy` **twice with the same seed**, run a fixed V-time
   horizon, `state_hash` — the two must be **bit-identical** (no `step_error`, no
   `skid`/`DIAG-SKID49`). A seal that will not branch deterministically is **reclassified a
   failure** *and* fails the gate loudly (a genuine seal regression is a determinism-core bug
   to escalate — a task-63 non-goal to patch here).
4. **Adversarial pass (§3)** — a **jittered** schedule (task 59's host-perturb path is not yet
   landed, so we vary the target `Moment`): run to each jittered boundary and seal. Jitter
   lands the guest at *different, busier* boundaries (more likely to carry in-flight
   injection), testing whether task-41 capture is **robust** — does sealing hold when
   perturbed into a less "convenient" state, or only at incidentally-quiescent points?
5. **Interior grid-probe (feeds §5)** — from each jittered boundary, step a deterministic
   little way in and seal at the **interior**. Interior points are usually *non-synchronized*
   (exact V-time is known only at intercepts), so these mostly fail — the negatives that make
   `sealable()`'s precision/recall non-trivial and that characterize the addressable grid.
6. **Materialization depth (§4)** — take the **deepest** sealed point as the child and its
   **nearest shallower** sealed ancestor as the parent; confirm the child materializes
   **bit-identically** by `branch(parent) → run the suffix` (and cross-check from genesis),
   and record the suffix-vs-genesis replay-depth ratio.
7. **Roll-up** — `SealStats` (nominal / adversarial / interior), `Overshoot`
   (addressability), `PredicateQuality` for `sealable` over all passes, `MaterializationDepth`,
   and the `Ruling`.

## 5. Substrate analysis (the load-bearing reasoning)

This section is **not** a simulation — it is what the code paths guarantee, and it is what
makes the projected result the *expected* one.

**(a) `run(deadline)` lands on a synchronized, sealable boundary by construction.**
`Vmm::effective_vns()` is `snapshot_vns(last_intercept_work)`, and `last_intercept_work` is
updated **only** at V-time-synchronized intercepts (`RDTSC`/`RDTSCP`/`RDRAND`/`RDSEED`/the
TSC MSRs and the preemption-timer arm-point); `Vmm::step()` clears `vtime_synchronized` on
entry and a V-time intercept re-sets it. The `run` loop (both the task-58 `ControlServer` and
this harness) checks `effective_vns()` *before* each step, so it stops at the **first
synchronized boundary at/after the deadline** — precisely the point where `save_vm_state`
does *not* fail closed on `!vtime_synchronized`. So the archive's materialization pattern
(`run(deadline) → seal`) targets sealable points by construction.

**(b) Task 41 removed the in-flight-injection failure class.** `save_vm_state` at a
synchronized boundary fails closed only for (i) a staged RNG completion (`rng_mid_exit`,
rare and *retryable* — step once to commit), or (ii) unrepresentable CPU state
(`kvm_sregs2` flags/pdptrs, `debugregs.flags`, `triple_fault`/exception-payload). For the
64-bit / paging-off determinism guest (ii) is effectively **never** (a triple fault is a
`KVM_EXIT_SHUTDOWN`; the payload cap is off; PAE PDPTRs unused; `debugregs.flags` defined
0) — it only closes the contract for synthetic/relayed blobs. Crucially, the **3112
in-flight-injection** boundaries task 40 rejected are now **captured** (the full
`kvm_vcpu_events` rides the device blob) — no longer a failure.

**(c) The residual limit is the non-synchronized *interior*, not a task-41 gap.** Task 40's
**5280 non-synchronized** class is the fundamental V-time-exactness limit: at a non-intercept
exit the retired work since the last intercept is not deterministically measurable (skid), so
the exact `vns` is unknown and `save_vm_state` fails closed. This is *inherent to V-time*, not
something task 41 addresses — and it is exactly why the archive addresses by **boundary**
(via `run(deadline)`), not by an exact interior `Moment`. The interior grid-probe (§5) exists
to quantify this, not to fail the gate.

**Net:** arbitrary-V-time sealing **to the nearest synchronized boundary** is expected to hold
at ~100 %, and (by task 41's own mid-workload determinism gates) to branch bit-identically.
The only open questions the box run answers are the **exact** nominal/adversarial rates
(does any busy-window boundary still fail to seal or to branch deterministically — a task-41
robustness gap?) and the **grid density** (the overshoot distribution → unrestricted vs
grid-restricted GO).

## 6. Projected result (model + analysis — NOT a box measurement)

Calibrated from §5 via the `vmm_core::seal_rate::mock` oracle (dense synchronized grid
≈ 2 048 ns; in-flight captured, not a failure; unrepresentable ≈ 0; determinism holds), for a
representative post-readiness span of ~300 M ns of V-time and **N = 64** (61 uniform + 3
busy). **Replace every value below with the harness output on the box.**

| Metric | Projected | Measured (box) |
|---|---|---|
| Nominal seal rate (§1) | **64/64 = 100.0000 %** | _pending_ |
| — failures | none (rare `rng-mid-exit` is retryable) | _pending_ |
| Branch-from-seal determinism (§2) | **64/64 bit-identical** (0 nondeterministic) | _pending_ |
| Adversarial seal rate (§3, jittered boundaries) | **≈ 100 %** (task-41 capture robust to busy boundaries) | _pending_ |
| Interior grid-probe seal rate (§5 negatives) | **low** (~most `non-synchronized`; expected) | _pending_ |
| Addressability — overshoot p90 (§ grid) | **~2 000 ns** (dense grid) | _pending_ |
| Materialization depth ratio (§4) | **≈ 1.59 %** (from_parent ≈ 4.7 M / from_genesis ≈ 295 M ns) | _pending_ |
| `sealable()` precision / recall (§5) | **100 % / 100 %** | _pending_ |

Notes:
- **Nominal 100 %** is the archive-relevant number: it is the rate at which `run(deadline) →
  seal` succeeds, which is exactly how Phase C materializes an exemplar.
- **Interior low is expected and healthy** — it confirms the addressable grid is quantized to
  synchronized boundaries. It is *not* a NO-GO input; the ruling is driven by the nominal +
  adversarial *boundary* rates and the determinism check.
- **Depth ratio ≈ 1.6 %** confirms the Phase-C premise directly: reconstructing a deep node
  from its nearest retained ancestor replays one inter-sample gap (~span/N), not the whole
  prefix from genesis — cost = suffix ≪ prefix. This ratio is the baseline **task 68 must beat
  live** (task 68 gate 3).

## 7. Runbook (what the foreman runs)

```sh
# On the box, in a checkout of this branch (task/validate-arbitrary-vtime-seal):
make -C guest fetch && make -C guest/linux postgres-image      # if the image isn't built
# load the patched kvm.ko / kvm-intel.ko for the running kernel, then (core 2 — the standing
# frontier-gate core; serialize with any other frontier gate):
taskset -c 2 timeout 7200 cargo test -p vmm-core --test seal_rate_sweep \
    -- --ignored --nocapture --test-threads=1 2>&1 | tee /tmp/seal_rate.log
# Transcribe the [REPORT] block into §6/§9 of this file and commit.
```

**Box-safety (CRITICAL).** Stock KVM = **1396736**; the patched module is larger. ALWAYS leave
the box on stock + verified after the run:

```sh
pkill -9 -f seal_rate_sweep            # FIRST (separate ssh call; expect exit 255 on drop)
#  wait until: lsmod | grep '^kvm_intel'   shows users=0
rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel
#  verify on a FRESH ssh connection:  lsmod | grep '^kvm '   == 1396736
```

Knobs: `TARGETS` (N, default 64), `BRANCH_HORIZON_VNS` (default 4_000_000), `ADV_JITTER_VNS`
(default 50_000), `ADV_PERTURB_STEPS` (default 4096), `WALL_BUDGET_SECS` (default 1800),
`SPAN_START`/`SPAN_END`/`BUSY_CENTERS` (skip profiling for fast re-runs), `BOOT_CMDLINE`.
Also refresh the box-only public-API snapshot after this branch's new `seal_rate` module:
`UPDATE_PUBLIC_API=1 cargo test -p vmm-core --test public_api -- --ignored` (nightly + tooling on the box).

## 8. Handoff

- **Portable gate (gate 1): DONE & GREEN** — `src/seal_rate/` (schedule, bookkeeping,
  `sealable`, ruling, mock) with 22 unit/proptest cases (512 cases each). No `/dev/kvm`, no
  float, no `HashMap`-into-output.
- **Box harness (gates 2–3 substrate): DONE** — `tests/seal_rate_sweep.rs`, additive, no new
  deps, `#![cfg(target_os="linux")]` + `#[ignore]` (empty binary on macOS). Needs a **Linux
  compile-check on the box** (it cannot compile on macOS: `boot_linux_selected` is
  Linux-gated). Reviewed against confirmed API signatures.
- **This measurement (gates 2–3 numbers + final ruling): HANDED TO THE FOREMAN** — run the
  runbook, transcribe the `[REPORT]` block here, and commit. Expected outcome: **GO** (see §9).
  A partial result (nominal < ~99 % or any nondeterministic seal) flips to **NO-GO /
  RESTRICTED** and Phase C inherits the `sealable(Moment)` predicate the harness emits.

## 9. The ruling

> **PROVISIONAL: GO** — pending the box run confirming nominal ≥ ~99 % and 100 % branch
> determinism.

Reasoning: arbitrary-V-time sealing **to the nearest synchronized boundary** holds at the rate
the archive relies on (§5(a),(b)); task 41's non-quiescent capture makes the previously-fatal
in-flight class sealable and (by its own gates) deterministic; the only *inherent* limit is
sealing at an exact non-boundary interior `Moment` (§5(c)), which the archive never needs —
it materializes via `run(deadline) → seal`, always landing on a boundary.

Whether the box confirms **GO (unrestricted)** or **GO (grid-restricted)** depends only on the
measured overshoot (boundary density): a dense grid (small p90 overshoot) means sealing is
effectively continuous; a coarse grid means exemplars key to the *nearest synchronized
boundary* — which `sealable()` accepts — rather than an exact interior `Moment`. **Either way
Phase C proceeds.** The single condition that would produce a genuine **NO-GO / RESTRICTED** is
a measured nominal/adversarial rate below the bar or any seal that fails to branch
deterministically — in which case Phase C inherits the emitted `sealable(cpu_snapshot)`
predicate (`synchronized ∧ ¬rng_mid_exit ∧ ¬unrepresentable`) with its measured
precision/recall, and the archive admits exemplars only at predicate-passing points.

_Hand this ruling to the foreman as the gate on Phase C (task 64 + the frontier
materialization task 68)._
