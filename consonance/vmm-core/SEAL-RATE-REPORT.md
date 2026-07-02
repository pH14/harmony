# SEAL-RATE-REPORT — arbitrary-V-time seal validation (task 63, the Wave-5 go/no-go)

> **Status: MEASURED on the determinism box (2026-07-02).** Gate 1 (portable
> sampling-schedule + seal-rate/`sealable` bookkeeping) is green on macOS + Linux; the box
> measurement (`tests/seal_rate_sweep.rs`, patched KVM 1400832, `taskset -c 2`, det-cfl-v1
> PASS) completed rc=0 and KVM was reverted to stock 1396736 (verified). Measured numbers are
> in [§6](#6-measured-result-box); the substrate analysis in [§5](#5-substrate-analysis-the-load-bearing-reasoning)
> is confirmed by them. **The final GO/NO-GO ruling is escalated to the integrator** (foreman
> ruling, 2026-07-02) — this report presents the measurement and the fork, not the verdict.
> Full box output is on PR #50 (comment `issuecomment-4867390481`).

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

## 6. Measured result (box)

Determinism box, patched KVM 1400832, `taskset -c 2`, det-cfl-v1 PASS. Post-readiness span
`[441861206, 463031443)` (≈ 21.17 M ns of V-time), **N = 64** (64 uniform; 0 busy windows
auto-detected in this workload's post-readiness phase). §2/§4 branch-verified on a spread
snapshot subset (9 this run + 17 in a prior full run = **26 sealed points branch-verified,
all bit-identical**). Raw `[REPORT]` on PR #50.

| Metric | Measured |
|---|---|
| **§1 nominal seal rate** (run→boundary→seal) | **64 / 64 = 100.0000 %** — no failures |
| **§2 branch-from-seal determinism** | **26/26 bit-identical** (9 + prior 17); 0 nondeterministic |
| §3 adversarial (jittered boundaries) | 34 / 56 = 60.7143 % (22 `non-synchronized`) |
| §5 interior grid-probe (non-boundary) | 24 / 55 = 43.6364 % (31 `non-synchronized`) |
| **§4 materialization premise** | **parent-rooted == genesis-rooted** (`2c71f9ab…`); ratio **1.6561 %** (from_parent 7 667 740 / from_genesis 462 999 204) → **savings 98.34 %** |
| **§4b schedule-faithful replay** | **MATCH** (`1c04e4cc…` == `1c04e4cc…`) — the probe/deadline schedule is a *deterministic* part of the trajectory |
| addressability — overshoot (ns) | min 7 · **p50 284 069 · p90 4 764 144** · max 6 748 854 · mean 1 260 128; exact_hits 0/64 |
| `sealable()` precision / recall | **TP 122 · FP 0 · TN 53 · FN 0 → 100 % / 100 %** |

Reading:
- **Boundary-addressed sealing is 100 % and 100 %-deterministic**, and **materialization is
  ancestor-independent** (§4 premise holds; cost = the **1.66 %** suffix, not the prefix — the
  baseline task 68 must beat live). This is the archive's actual pattern (`run(deadline)→seal`).
- **§4b MATCH ⇒ the substrate is sound**: the live trajectory reproduces bit-for-bit when
  replayed with the same probe/deadline schedule. The live-vs-clean-replay divergence
  (`1c04e4cc` vs `2c71f9ab`) is a *deterministic, reproducible* schedule effect, not
  non-reproducible perturbation.
- **But sealing at an *exact arbitrary interior* V-time is not addressable**: interior /
  adversarial points fail ~40–60 % (`non-synchronized`), and — correcting §5's projection —
  the boundary grid is **coarse, not dense** (p90 overshoot ≈ 4.76 M ns ≈ 22 % of the span; 0
  targets hit exactly). You can seal only at the nearest synchronized boundary, which can be
  millions of ns past the requested `Moment`.
- The `sealable()` predicate separates boundary (sealable) from interior (non-sealable) points
  **perfectly (100 % / 100 %)** over the 175-point union — it cleanly keys the Phase-A2 set.

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

Knobs: `TARGETS` (N, default 64), `DET_SUBSET` (spread seals given the full 2 GiB snapshot +
§2/§4 branch-verify, default 24), `BRANCH_HORIZON_VNS` (default 4_000_000), `ADV_JITTER_VNS`
(default 50_000), `ADV_PERTURB_STEPS` (default 32), `WALL_BUDGET_SECS` (per **guest**, default
1800), `SPAN_START`/`SPAN_END`/`BUSY_CENTERS` (skip profiling for fast re-runs), `BOOT_CMDLINE`.
This report's run used `SPAN_START=441861206 SPAN_END=463031443 DET_SUBSET=8
BRANCH_HORIZON_VNS=1000000` (the 2 GiB memory ops — `state_hash`/`snapshot_base`/`materialize`/
`restore` — are ~30–60 s each on this box, so keep `DET_SUBSET` modest and budget generously).
Also refresh the box-only public-API snapshot for the new `seal_rate` module:
`UPDATE_PUBLIC_API=1 cargo test -p vmm-core --test public_api -- --ignored` (nightly + tooling on the box).

## 8. Handoff

- **Portable gate (gate 1): DONE & GREEN** — `src/seal_rate/` (schedule, bookkeeping,
  `sealable`, ruling, mock) with 22 unit/proptest cases (512 cases each). No `/dev/kvm`, no
  float, no `HashMap`-into-output.
- **Box harness (gates 2–3 substrate): DONE & RUN** — `tests/seal_rate_sweep.rs`, additive, no
  new deps. Compiled + clippy-clean + executed on the box (rc=0); KVM reverted to stock.
- **Measurement (gate 2 numbers): DONE** — see §6. **Final ruling (gate 3): escalated to the
  integrator** (foreman ruling 2026-07-02) — §9 presents the fork, not the verdict.
- Still open: refresh the box-only public-API snapshot for the new `seal_rate` module
  (`UPDATE_PUBLIC_API=1 cargo test -p vmm-core --test public_api -- --ignored`).

## 9. The fork (ruling escalated to the integrator)

The measurement is **not** the clean GO the projection expected, nor a substrate failure. It
splits along one axis:

**Substrate soundness — PASS.** §1 nominal 100 %, §2 determinism 26/26, §4 premise holds
(materialization ancestor-independent, cost = 1.66 % suffix), **§4b MATCH** (the live
trajectory reproduces bit-for-bit under the same probe/deadline schedule). No task-41/63
determinism-core regression. Task 41 genuinely cleared the "0 of 8392" barrier.

**Addressability — RESTRICTED.** Sealing works at the **nearest synchronized boundary**
(100 %), but **not at an exact arbitrary interior `Moment`** (interior/adversarial ~40–60 %
`non-synchronized`), and the boundary grid is **coarse** (p90 overshoot ≈ 4.76 M ns ≈ 22 % of
the span; 0/64 exact hits). This is the fundamental V-time-exactness limit, not a task-41 gap.

So the two admissible readings the integrator chooses between:

- **GO (boundary-keyed)** — Phase C keys exemplars to the *nearest synchronized boundary*
  (which `run(deadline)→seal` lands on by construction and `sealable()` accepts at 100 %/100 %).
  Materialization is sound (§4/§4b). The coarse grid means an exemplar's `Moment` snaps to its
  nearest boundary — acceptable if the archive addresses by boundary, not exact interior V-time.
- **NO-GO / RESTRICTED** — if Phase C requires exact-interior-`Moment` addressing, it must key
  exemplars to `sealable(Moment) = synchronized ∧ ¬rng_mid_exit ∧ ¬unrepresentable` (measured
  precision/recall **100 %/100 %**), refuse admission at non-`sealable` points, and carry the
  **§4b rider**: to reproduce a probe-laden trajectory, materialize probe-free or replay the
  exact `run(deadline)`+probe schedule (both deterministic).

The mechanical threshold summary the harness prints is **NO-GO / RESTRICTED** (driven by the
coarse grid + sub-threshold adversarial rate). **The integrator makes the Phase-C call.**
