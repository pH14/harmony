# Task 84 — the fault-free exploration gate + game-shaped benchmark

> **FRONTIER · the exploration seam's own gate (the Metroid discipline) — and the task-70
> on-ramp.** `docs/LAYERS.md` R-L1 rules that exploration is a first-class, independently-gated
> capability: the searcher is useful, and must be validated, **with zero faults**. This task
> builds the gate LAYERS names — *cells discovered / depth reached vs. a random-seed baseline,
> zero faults (`FaultPolicy::none()`, buggify off), on a real guest, driving the real spine
> engine (`explorer::Explorer` + `SocketMachine`)* — over a **game-shaped benchmark workload**: a
> small deterministic maze under the Linux guest whose position is reported through SDK state
> registers (the `sdk-demo` pattern grown up). It is deliberately also the **first real-hardware
> run of the composed engine**: every box campaign to date drove conductor's hand-rolled loop, so
> this is the on-ramp task 70 (`dissonance/selector-bandit`) builds its beats-baseline gate on.
>
> Depends on **task 58** (control server + `SocketMachine`), **task 60** (the campaign harness +
> guest-workload-init pattern this reuses), **task 64** (the spine: `Selector`/`Archive`/`CellFn`/
> `Sensor`), **task 67/link** (`LinkSensor` — the state-register→feature channel this workload's
> cells key on), **task 68** (materialization, so the campaign actually branches from archive
> cells), and **task 73** (the guest SDK — `state_set`/`entropy_fill`). Independent of tasks 61
> and 69's fault machinery by construction — it removes fault *enforcement* (the half-finished
> part) from the equation entirely.

Read first: `tasks/00-CONVENTIONS.md`, `docs/LAYERS.md` (R-L1, R-L2 + its Metroid corollary, and
the one-reproducer constraint), `docs/EXPLORATION.md` (the `quiet` tactic arm, "the two hard
problems", roadmap rows E/F), `tasks/60-first-campaign-planted-bug.md` (the campaign + guest-init
pattern this extends), `tasks/69-signal-bug-correlation.md` (the trial discipline and report
format this mirrors), `tasks/70-selector-bandit.md` (the task this is the on-ramp for),
`guest/payloads/sdk-demo/src/main.rs` (the SDK-instrumented-payload pattern grown up here),
`dissonance/explorer/src/adapter.rs` (`SocketMachine`, `SpecEnvCodec`),
`dissonance/explorer/src/engine.rs` (`Explorer`), `dissonance/link/src/sensor.rs` (`LinkSensor`,
`LINK_STATE_CHANNEL`), `dissonance/explorer/src/stads.rs` (the discovery-curve estimator reused
here).

## Environment

Portable-logic surface: the maze/game logic, the exploration-metric bookkeeping (distinct-cell
and depth accounting), and the baseline comparison statistics are pure and macOS+Linux-testable.
The campaigns are **box-only** (patched KVM, the built Linux guest image carrying the game
workload). Pin per `docs/BOX-PINNING.md`; always revert KVM to stock **1396736** and verify after
any patched run (see Box-safety).

Surface list (frontier waiver of hard rule 1):

- `guest/` — the new **maze game** workload payload + init wiring, beside the `sdk-demo` and
  task-60 planted-bug patterns (follow `guest/linux/pg-init.sh` workload-init conventions). SDK
  state registers report position; input decisions are drawn from the seeded entropy stream
  (`Sdk::entropy_fill`, the project's single guest-random source). **Zero fault vocabulary** —
  no buggify site, no `assert_*` violation is required for this gate (an `assert_reachable` at a
  deep goal is permitted as a legibility marker, not a bug).
- `dissonance/benchmark/` — extend the task-69 measurement crate (do **not** fork a new one) with
  the fault-free exploration measures: distinct cells discovered at a fixed branch budget, depth
  reached, and the signal-vs-baseline comparison + report generation. Pure logic, whitelist-only
  deps; the order statistics (median, IQR) are hand-rolled over integers — no stats crates,
  reusing task 69's rank machinery where it fits.
- `consonance/vmm-core` — the **exploration campaign**: extend the task-58/60 campaign harness to
  drive a campaign through the composed `explorer::Explorer` + `SocketMachine` (the on-ramp — swap
  conductor's hand-rolled loop for the real engine) under `FaultPolicy::none()` with buggify off,
  `LinkSensor` feeding a `CellFn` keyed on the position state-registers, the default **v1**
  `Selector` and `CoverageArchive`; and emit the per-branch discovery-event log (branch index,
  cumulative distinct cells, depth reached, per-branch terminal `state_hash`) that
  `dissonance/benchmark` analyzes offline. A `--baseline` flag selects the control configuration
  (below).
- `dissonance/explorer`, `dissonance/link`, `guest/sdk` (`harmony-sdk`): **read-only** — composed
  and reused, never modified. This gate is the composed engine's first real-hardware exercise; if
  it surfaces a spine defect, that is a **finding to escalate**, not a spine change to smuggle
  into this task's surface.

## Context

`docs/LAYERS.md` R-L1: the spine is fault-blind by construction, so a campaign under
`FaultPolicy::none()` is a coverage-guided, snapshot-branching fuzzer over the payload/entropy
channels with perfect reproducibility — the tactic portfolio's `quiet` arm. Antithesis validate
their exploration engine exactly this way — on a game (Metroid), **no fault injection anywhere**,
position tuples fed to a `SOMETIMES_EACH` assertion, entropy purely from stochastic input
decisions. Under harmony's thin-SDK ruling (R-L2 corollary) the same shape is expressed host-side:
the app emits `state_set(x)` / `state_set(y)`, `LinkSensor` turns each into a `(Moment, Feature)`
on `LINK_STATE_CHANNEL`, and the campaign's `CellFn` keys cells on those channels — retunable
without recompiling the guest. This task is the end-to-end proof of that path on real hardware,
and the number task 70 must beat.

## Prior art

- **Antithesis, "Testing Exploration via Metroid" (2025)**
  <https://antithesis.com/blog/2025/metroid/> — the discipline this gate imports: exploration
  quality is measurable, and worth measuring, **decoupled from fault quality**; the assert
  vocabulary is the workload→searcher guidance interface and it works faults-off.
- **STADS** (Böhme, TOSEM 2018) [eng] — fuzzing as species discovery; species-accumulation curves
  + Good–Turing/Chao1 answer "is discovery still live, and how much is left". Reuse
  `explorer::stads` to render the campaign's discovery curve and its exhaustion signal.
- **Klees et al., "Evaluating Fuzz Testing"** (CCS 2018) [eng] — the trial discipline: measure
  against ground truth, run enough trials, report medians + variance, never single-run anecdotes.

## The workload — a deterministic maze (sub-question 1, ruled here)

**LAYERS R-L1 leaves the workload choice to this spec. Ruled: a small deterministic grid maze.**

A single supervised process in the Linux guest image walks an `H×W` integer grid from a fixed
start toward a goal. Each step it draws one byte of decision entropy via `Sdk::entropy_fill` (the
seeded stream — so a run is a pure function of the campaign seed) and moves accordingly, subject to
walls. It reports position through two IJON registers every step:
`state_set(REG_X, x)` / `state_set(REG_Y, y)` — the position markers `LinkSensor` turns into
cells. A distinguished deep tile emits `assert_reachable(GOAL)` as a legibility marker.

The maze is **not open** — it is structured so that random input plateaus while frontier-branching
progresses (the Metroid property, and what makes the gate non-vacuous, below):

- Reaching depth *d* (distance from start) requires an approximately *d*-length run of specific
  correct moves; a wrong move at a junction returns to a dead-end/reset. So the probability that
  an independent seeded run reaches depth *d* decays geometrically in *d* — **pure random restart
  is exponential in depth**, while a campaign that snapshots a deep cell and branches fresh entropy
  from it is roughly linear (this is the whole point of coverage-guided snapshot branching).
- The reachable frontier is **large enough that random seeds plateau well short of it** — document
  the total reachable cell count and the empirical random-seed plateau in the workload's
  `IMPLEMENTATION.md`, so "signal beats baseline" cannot pass vacuously via a baseline that had
  already saturated the space.
- Depth, grid size, and junction branching factor are **tunable manifest parameters** so the
  campaign completes on the box (target the signal config reaching the goal within ~10²–10³
  branches while the baseline does not).

Determinism discipline (rule 4): grid coordinates, distinct-cell counts, and depth are integers;
the median/IQR are integer order statistics; no `HashMap`/`HashSet` iteration reaches the log, a
hash, or the report. Floats may appear only in rendered report statistics, never in campaign
state, the archive, or any `state_hash`.

## The baseline (sub-question 2, ruled here)

**LAYERS R-L1 offers "pure random seeds vs. frontier-off"; this spec must pick. Ruled:**

- **Primary baseline — pure random seeds.** *N* independent campaign seeds, each run once from
  genesis with **no archive branching** (random-restart search: the composed engine with the
  frontier held empty, or equivalently the task-60 blind-seed loop). This is the true null for
  "does coverage-guided snapshot exploration beat luck," it mirrors Antithesis's random-input
  comparison and task 69's blind-seed baseline, and it is what the maze's geometric depth-decay is
  designed to defeat. **The gate is scored against this.**
- **Secondary/diagnostic control — frontier-off.** The composed engine runs and snapshots, but the
  `Selector` ignores novelty (branches from genesis, never exploits an admitted cell). Reported
  alongside to **separate two contributions** — snapshot-branching itself vs. novelty-steering —
  so a FAIL can be attributed. Not the pass/fail line; a diagnostic column in the report.

Both controls run at the **identical branch budget** as the signal configuration.

## Acceptance gates

1. **Portable (macOS + Linux):** the maze logic unit-tested (a fixed entropy stream drives a
   fixed path; walls/resets behave; the reachable-cell count and random-seed plateau are asserted
   against the manifest so the non-vacuity claim is checked, not just prose); the discovery/depth
   bookkeeping deterministic; the baseline-comparison order statistics proptested (≥256) against
   synthetic distributions of known median/IQR. Standard suite green on every touched crate
   (`build` / `nextest` / `clippy -D warnings` / `fmt` / `cargo deny`).
2. **Box gate — the on-ramp (determinism):** a campaign driven end-to-end by the composed
   `explorer::Explorer` + `SocketMachine` on real KVM, `FaultPolicy::none()` + buggify off,
   **replays bit-identically**: the same campaign seed reproduces the identical per-branch
   `state_hash` sequence **25/25**, and one discovered deep cell's reproducer replays its terminal
   `state_hash` **25/25**. This is the first real-hardware run of the composed engine — record it
   explicitly. (Per `docs/BOX-PINNING.md` co-tenancy directives, a solo vs. co-tenant `state_hash`
   divergence is a **P0 STOP + escalate**, never serialize-to-hide.)
3. **Box gate — the exploration measurement:** a committed
   `dissonance/benchmark/EXPLORATION-GATE-REPORT.md` over **≥20 seeds per configuration** (signal,
   pure-random baseline, frontier-off diagnostic), medians + IQR, plus the STADS discovery curve
   (`explorer::stads`) for the signal configuration and its exhaustion signal at the budget.
4. **The pass condition (the gate itself):** at the fixed branch budget, on the maze workload, the
   **signal configuration strictly beats the pure-random baseline** on **both** distinct cells
   discovered **and** depth reached — greater medians with non-overlapping IQRs (state the minimum
   effect in the report) — while the baseline demonstrably still explores (non-zero, below the
   documented reachable frontier, so the win is real and not a broken control). Zero faults were in
   play the whole time (`FaultPolicy::none()`, buggify off — the report records it: the `quiet`
   arm). A **PASS** ratifies the exploration seam and hands task 70 its baseline numbers + the maze
   fixture; a **FAIL** (signal does not beat random, or the maze is vacuous/too easy) routes to the
   `CellFn`/workload — **not** to search cleverness (task 70 is not the fix, exactly as task 69
   gates it). Hand the report + verdict to the foreman.

## Box-safety (CRITICAL)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock + verified
after every run: `pkill -9 -f` the campaign bin (and any `live_*`) FIRST → wait
`lsmod | grep '^kvm_intel'` users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` →
verify size 1396736 on a FRESH ssh connection. SSH drops (exit 255) on pkill/rmmod are normal —
reconnect + verify. Pin builds/tests to a leased core (`taskset -c`, `docs/BOX-PINNING.md`). Run
gates in the foreground and READ results before reporting; no detached pollers + idle.

## Non-goals

- **Any `Selector`/search cleverness** — that is task 70, gated on this PASS. This task runs the
  default v1 Selector only; it measures the seam, it does not improve it.
- **Any fault vocabulary** — no `HostFault`, no buggify-gated bug, no fault enforcement (net 61b,
  block/process). Removing enforcement from the equation is the point (R-L1).
- **Fattening the SDK** — no app-declared cell functions, no new verbs; the host owns the cell
  interpretation (R-L2). The workload uses only existing `state_set`/`entropy_fill`/
  `assert_reachable`.
- **Modifying the spine/engine** — `explorer`/`link`/`harmony-sdk` are read-only; a spine defect
  this surfaces is escalated, not patched here.
- **Multi-objective archive preference** (the "prefer more missiles" gap R-L2 logs) — a task-70
  design input, not built here.
- **Physically relaying out any crate** (the R-L3/R-L4 moves) — those ride task 43 / issue #74.
