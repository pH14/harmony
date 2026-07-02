# Task 63 — validate non-quiescent seal at arbitrary V-time (the Wave-5 go/no-go)

> **FRONTIER · the Wave-5 go/no-go.** The whole exploration archive (`docs/EXPLORATION.md`) rests
> on one empirical assumption: task 41 lets you seal a snapshot at an *arbitrary* mid-workload
> `Moment` and branch from it deterministically — not just at the handful of quiescent boundaries.
> Task 40 found **0 of 8392** post-readiness boundaries sealable under the old quiescent-only codec.
> This task adversarially measures whether task 41 actually cleared that, because parent-rooted lazy
> materialization is unbuildable if it did not. **Diagnose and measure; do not build the archive.**
>
> Depends on **task 41** (non-quiescent snapshot) and **task 58** (the server/`Machine` that serves
> `snapshot`/`branch`/`replay`/`run` against a live guest). Independent of 59/60/61.

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` ("Navigation seam" + "Phase A"),
`docs/DISSONANCE.md` ("The two loops" — snapshot is Progression navigation, `snapshot` at any V-time
point), `tasks/41-non-quiescent-snapshot.md`, `consonance/vmm-core/tests/live_branching_demo.rs`
(the hand-driven seal/branch demo — the base to extend), `consonance/vmm-core/src/vmm.rs`
(`save_vm_state`/`restore_snapshot`/`state_hash`, the snapshot path).

## Environment

Portable-logic surface: the V-time sampling schedule and the seal-rate/`sealable`-predicate
bookkeeping are pure and macOS+Linux-testable against a mock. The measurement itself is **box-only**
(patched KVM, the built Postgres image; a real workload must run to mid-execution). Pin per
`docs/BOX-PINNING.md`; always revert KVM to stock **1396736** and verify after any patched run.

Surface list (frontier waiver of hard rule 1): `consonance/vmm-core` (a new
`tests/seal_rate_sweep.rs` + any read-only diagnostics), plus read access to the snapshot path.
No production behavior changes — this is a measurement harness.

## Context

`live_branching_demo.rs` proves the seal→branch mechanism hand-driven at a single point; task 41
claims to have lifted the quiescent-only limit by capturing in-flight CPU event/interrupt state.
Neither established the **rate** at which arbitrary V-times are sealable under a real, never-halting
workload, nor whether a mid-workload seal survives adversarial timing perturbation. `docs/EXPLORATION.md`
needs both numbers to commit to the archive design; if the rate is low, the archive must restrict
cells to a `sealable(V-time)` set (Phase A2).

## What to measure

### 1. Sample a spread of mid-workload V-times

Run the Postgres workload (the `live_runc_postgres` or `live_branching_demo` guest) past
`GUEST_READY`. Choose **N ≥ 64** target `Moment`s spread across the post-readiness run (uniform in
retired-instruction count, plus a handful landing deliberately inside known-busy windows — interrupt
service, WAL fsync, scheduler ticks). At each: `run` to the target `Moment`, `snapshot`, record
success/failure and the reason on failure.

### 2. Prove each seal is a real branch point

For every **successful** seal: `branch(s, env)` twice with the same seed, `run` a fixed V-time
horizon past the seal, and `hash` — the two must be **bit-identical** (`state_hash` equal,
`step_error=None`, zero `skid exceeded`/`DIAG-SKID49`). A seal that cannot be branched
deterministically counts as a **failure**, not a success.

### 3. Adversarial pass

Repeat step 1 with an `InjectInterrupt`-style timing perturbation staged just before each target
`Moment` (borrow the host-perturb path if task 59 has landed; otherwise vary the target `Moment` by
a small jitter). The question: does sealability hold when the guest is perturbed into a less
"convenient" state, or does it only work at incidentally-quiescent points?

### 4. Measure materialization depth (the parent-rooted premise)

For a subset, seal a *second* snapshot deeper in the run, then materialize it by
`branch(shallower_seal) → run` the suffix rather than replaying from genesis. Record replay depth
(retired instructions from parent vs. from genesis). This confirms the Phase-C premise that
materialization cost is the *suffix*, not the whole prefix.

### 5. If sealability is partial: define `sealable(Moment)`

If the success rate is below 100%, characterize *which* states fail (RIP class, IF, armed-timer
state, in-hypercall) and emit a pure predicate `sealable(cpu_snapshot) -> bool` that matches the
observed successes, plus its measured precision/recall over the sweep. This predicate is the Phase-A2
deliverable that the `Archive` will consult so it only admits exemplars at materializable points.

## Prior art

- **Agamotto** (Song et al., USENIX Sec 2020) — the closest prior art: its contribution is
  dynamically choosing checkpoint locations during a run ("checkpoint only where it pays"). This
  task measures whether the substrate even *permits* the arbitrary-location seals Agamotto assumes;
  if sealability is partial, its cost-aware placement framing defines the §5 `sealable(Moment)`
  predicate the Archive will consult.

## Acceptance gates

1. **Portable (macOS + Linux):** the V-time sampling schedule and the seal-rate / `sealable`
   bookkeeping have unit + proptest (≥256) coverage against a mock snapshot oracle; standard suite
   green on `vmm-core`'s touched logic.
2. **Box gate — the measurement (the go/no-go result):** a committed report
   (`consonance/vmm-core/SEAL-RATE-REPORT.md`) giving, over N ≥ 64 target `Moment`s: the seal-success
   rate (nominal and adversarial), the branch-from-seal determinism result per successful seal
   (must be 100% bit-identical or the seal is reclassified a failure), the measured materialization
   depth ratio, and — if <100% — the `sealable` predicate with its precision/recall.
3. **The ruling.** The report ends with an explicit **GO** (arbitrary-V-time sealing holds at a rate
   the archive can rely on — Phase C proceeds unrestricted) or **NO-GO / RESTRICTED** (the archive
   must key exemplars to `sealable(Moment)` — Phase C inherits the predicate). Hand this to the
   foreman as the gate on Phase C.

## Box-safety (CRITICAL)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock + verified after
every run: `pkill -9 -f seal_rate_sweep` (and any `live_*`) FIRST → wait `lsmod | grep '^kvm_intel'`
users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` → verify size 1396736 on a FRESH
ssh connection. SSH drops (exit 255) on pkill/rmmod are normal — reconnect + verify. Pin builds/tests
to `taskset -c 2` (`docs/BOX-PINNING.md`). Run gates in the foreground and READ results before
reporting; no detached pollers + idle.

## Non-goals

- Building the `Archive`, `Selector`, or materialization engine — that is Phase C (task 64 + the
  frontier materialization task). This task only *measures* and, if needed, *hands them a predicate*.
- Fixing task 41 if it regresses — escalate to the foreman; a genuine seal regression is a
  determinism-core bug, not this harness's to patch.
- Optimizing seal/branch latency — correctness and rate first.
