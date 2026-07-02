# Task 69 — seeded-bug benchmark + signal→bug correlation harness (GO/NO-GO #2)

> **FRONTIER · GO/NO-GO #2 — the gate on Phase F.** `docs/EXPLORATION.md`'s second hard problem:
> a feedback signal that does not correlate with bugs makes a better search optimize the wrong
> thing faster. This task extends task 60's single planted bug into a seeded-bug **benchmark**
> (≥3 bugs, distinct classes) and measures whether the Phase-D signal stack's cell/feature
> novelty correlates with progress toward them. **Nothing in Phase F (task 70+) is built until
> this report says GO; if it says NO-GO, the fix is the cell function (iterate task 67), never
> the search.** Measure and rule; do not build Selectors.
>
> Depends on **task 60** (the first planted bug — reused verbatim), **task 65** (`RunTrace`
> recorder), **task 67** (sensors + CellFn v1 — the signal under test), **task 68** (lazy
> materialization, so campaigns actually branch from archive cells). Independent of task 61;
> task 66 enters only through 67's sensors. The planted-bug payloads are parallelizable early
> (EXPLORATION's off-path E1 — buildable from Phase B onward); the measurement needs 65/67/68.

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` ("The two hard problems" + roadmap
rows E and F), `tasks/60-first-campaign-planted-bug.md` (the pattern this extends),
`tasks/63-validate-arbitrary-vtime-seal.md` (the ruling format this report mirrors),
`dissonance/explorer/src/spine.rs` (`Archive`/`Frontier`/`CellKey`/`Reward` — task 64), and the
landed specs for tasks 65/67/68.

## Environment

Portable-logic surface: planted-bug trigger logic, the STADS estimators, and the correlation
bookkeeping are pure and macOS+Linux-testable. The campaigns are **box-only** (patched KVM, the
built Postgres workload image). Pin per `docs/BOX-PINNING.md`; always revert KVM to stock
**1396736** and verify after any patched run.

Surface list (frontier waiver of hard rule 1):

- `guest/` — the two new planted-bug payloads + init wiring, beside task 60's (follow
  `guest/linux/pg-init.sh` workload-init conventions).
- `dissonance/benchmark/` — **new crate**: the benchmark manifest (bugs, trigger thresholds,
  serial markers), the correlation statistics, and report generation. Pure logic; whitelist-only
  deps — the correlation statistics are hand-rolled (rank/Spearman math over integers), no
  stats crates.
- `dissonance/explorer/src/stads.rs` — **new module** (the one explorer touch): species
  accumulation + Good–Turing/Chao1 over an opaque cell-discovery event stream. Progression-blind
  by construction — it folds counts of opaque `CellKey` discoveries, nothing else.
  Integer/rational arithmetic only — Good–Turing as the fraction `f1/n`, Chao1 via
  cross-multiplied comparisons; floats may appear only in report rendering, never in the
  estimator. **Task 70's Selector v3 consumes this via its existing `explorer` dependency for
  state-affecting policy; this location is a contract.**
- `consonance/vmm-core` — extend the task-60 campaign bin to drive benchmark campaigns and emit a
  per-branch discovery-event log (branch index, cumulative distinct cells, per-bug find branch)
  that `dissonance/benchmark` analyzes offline.

## Context

Task 60 proved the loop finds one planted bug and replays it 25/25 — with blind seed search.
Phases C/D (tasks 64–68) added the signal stack: `RunTrace` → Sensor → CellFn v1 → cell
`Archive` → materialized branching from cells. The unanimous failure mode in the literature is
not the search algorithm but a signal that doesn't track bugs — which is why Phase E hard-gates
Phase F. This task is the Klees/STADS instrumentation that decides, with ground truth, whether
the signal earned a smarter search.

## Prior art

- **STADS** (Böhme, TOSEM 2018) [eng] — fuzzing as species discovery: species-accumulation
  curves + Good–Turing/Chao1 estimators answer "is the signal still discovering, and how much is
  left". This task's correlation instrument and the wave's prototype stopping rule.
- **Klees et al., "Evaluating Fuzz Testing"** (CCS 2018) [eng] — how fuzzing evaluations fool
  themselves: measure against ground-truth bugs, run enough trials, report medians + variance,
  never coverage-proxy bug counts. The discipline this whole gate enforces.

## The benchmark (the shared fixture tasks 71/72 extend)

Three bugs now, of distinct classes:

1. **Fault-timing crash** — task 60's planted bug, reused verbatim. Do not rebuild it.
2. **Ordering/interrupt-timing** — fires only when an `InjectInterrupt`-timing perturbation
   (task 59 vocabulary) lands inside a vulnerable window: an ordering assumption in a small
   supervised process (e.g. a handler that corrupts shared bookkeeping if preempted mid-update).
3. **Rare-entropy-value** — a branch taken only on a rare seeded-entropy value (the task-42
   pattern, e.g. a `gen_random_uuid()` prefix match) that then poisons state and crashes.

Later tasks extend the fixture: **(iv)** a partition-duration bug (fires only when a partition
outlasts a lease/timeout window) — task **72's** portfolio box gate, via its fault-regime arm,
additionally requiring task 61 (standing net faults); **(v)** a depth-2 concurrency/ordering bug
(two ordered scheduling perturbations) — task 72's PCT gate; **(vi)** a planted
convergence/liveness failure — non-crashing (e.g. a supervised process that permanently stops
making progress after a specific fault burst but does not die), observable from the recorded
history or a forward probe — built by **task 75** under the same conventions as (iv)/(v). Design
the manifest so those slot in without restructuring; this benchmark is the shared fixture for
every later beats-baseline gate.

Every bug inherits task 60's requirements: deterministically triggerable (right
`(seed, fault schedule)` ⇒ fires every time; nominal ⇒ never), crash-observable via a
**distinct per-bug serial marker** → `StopReason::Crash` (so fingerprints attribute finds
per-bug), and documented trigger conditions + expected naive time-to-find. Trigger thresholds
are **tunable** manifest/init parameters (window width, retry count, prefix length) so expected
time-to-find dials into ~10²–10³ branches — campaigns must finish on the box.

## The correlation harness

Two configurations, identical budgets: **signal** (Phase-D stack — 65 RunTraces → 67 sensors +
CellFn v1 → 64 Archive with the default v1 Selector → 68 materialization) and **baseline**
(task 60's blind seed search). **≥20 seeds per configuration** (Klees-style trial discipline).
From each campaign's discovery-event log, measure:

1. **Novelty↔progress across seeds** — rank correlation (Spearman) between cells discovered at a
   fixed budget and time-to-bug, per bug: does a run that discovers more cells find bugs sooner?
2. **Trajectory** — for each find, does the finding run's ancestor chain pass through novel-cell
   admissions at an above-chance rate (the path to the bug runs through novelty)?
3. **STADS instrumentation** — species-accumulation curves (species = cells, samples =
   branches), Good–Turing discovery probability, Chao1 richness: was discovery still live when
   each bug fired, and how much is estimated left. Prototype the stopping rule ("stop when
   estimated discovery probability < ε") — task 70's Selector v3 consumes this estimator.
4. **Baseline comparison** — median time-to-bug (branches) + variance (IQR), signal vs baseline,
   per bug.

Report medians + variance everywhere; never single-run anecdotes. Floats are confined to
report-side statistics — nothing float-derived enters campaign state, hashes, or the archive
(conventions rule 4; rank statistics are integer-friendly anyway).

## Acceptance gates

1. **Portable (macOS + Linux):** each planted bug's trigger logic unit-tested against the
   mock/toy path (trigger schedule fires 100%, nominal never); STADS estimators proptested
   (≥256) against synthetic species distributions of known richness; correlation bookkeeping
   deterministic; standard suite green on all touched crates.
2. **Box gate — benchmark validity:** each of the ≥3 bugs is found by a campaign, and the
   emitted reproducer replays the identical crash (same `state_hash` at the terminal stop)
   **25/25**; a nominal-seed control run crashes on none; per-bug serial markers attribute every
   find unambiguously.
3. **Box gate — the measurement:** a committed `dissonance/benchmark/CORRELATION-REPORT.md` with
   all four measures over ≥20 seeds per configuration, medians + variance, and the species
   curves for both configurations.
4. **The ruling (mirror task 63).** The report ends with an explicit **GO** (cell novelty
   correlates with bug progress — right direction and meaningful effect size on ≥2 of the 3
   bugs, and the signal configuration's median is not worse than baseline on any bug) → Phase F
   / task 70 dispatches; or **NO-GO** (correlation absent or inverted) → iterate the CellFn
   (task 67) and re-run this harness — **the search is not the fix**. Hand this to the foreman
   as the gate on Phase F.

## Box-safety (CRITICAL)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock + verified
after every run: `pkill -9 -f` the campaign bin (and any `live_*`) FIRST → wait
`lsmod | grep '^kvm_intel'` users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` →
verify size 1396736 on a FRESH ssh connection. SSH drops (exit 255) on pkill/rmmod are normal —
reconnect + verify. Pin builds/tests to `taskset -c 2` (`docs/BOX-PINNING.md`). Run gates in the
foreground and READ results before reporting; no detached pollers + idle.

## Non-goals

- Selector v2/v3 or any search-policy work — that is task 70, gated on this GO.
- Building bugs (iv)/(v)/(vi) — tasks 72/75 extend the fixture; the manifest here just must not
  preclude them.
- Fixing a weak CellFn in this task — a NO-GO routes to a task-67 iteration, then re-measure.
- Triage/minimization; net-fault bugs (task 61); real (non-planted) bug hunts.
