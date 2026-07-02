# Task 70 — `dissonance/selector-bandit`: Selector v2 (count-based) + v3 (bandit + STADS stop)

> **FRONTIER (portable-heavy: the crate itself is Mac-gated; one box gate) · HARD-GATED on
> task 69 = GO.** Phase F of `docs/EXPLORATION.md`: the first real search cleverness, built only
> because Phase E validated that cell novelty correlates with bugs. Two `Selector` policies over
> task 64's spine — **v2** Go-Explore count-based weighting, **v3** an EcoFuzz-shaped
> non-stationary bandit with a STADS exhaustion/stopping signal. The Progression stays blind:
> both see opaque `CellKey`s and `Reward`s, never cell meaning.
>
> Depends on **task 64** (the spine: `Selector`/`Frontier`/`Reward` in
> `dissonance/explorer/src/spine.rs`) and **task 69 = GO** (the benchmark the box gate runs on,
> plus the STADS estimator this crate reuses). Do not dispatch before 69's
> `CORRELATION-REPORT.md` rules **GO** — "don't build past a GO/NO-GO without passing it"
> (`docs/EXPLORATION.md`).

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` (roadmap row F, "The two hard
problems", the Scoring seam), `tasks/64-explorer-spine-refactor.md` (the `Selector` contract +
the Progression-blindness invariant), `tasks/69-signal-bug-correlation.md` (the benchmark, the
trial discipline, and `stads.rs`), `dissonance/explorer/src/spine.rs`,
`dissonance/explorer/src/stads.rs` (the estimator this crate consumes).

## Environment

New crate `dissonance/selector-bandit/` — pure logic, macOS + Linux, laptop-gated. Its only
sibling dependency is `dissonance/explorer` (the sanctioned plugin dependency: it implements the
spine traits and reuses `explorer::stads`), plus the conventions whitelist. One acceptance gate
(gate 4, beats-baseline) is **box-only**: it runs the task-69 benchmark through the task-69
campaign harness; everything else closes locally.

Surface list: `dissonance/selector-bandit/` (the new crate — portable), plus the task-69
campaign bin's selector-flag wiring in `consonance/vmm-core` (naming which `Selector` a campaign
runs — the box gate's harness hook); read-only everywhere else.

## Context

Task 64 decomposed `Strategy` into `Tactic` + `Selector` behavior-preservingly, so the default
**v1** `Selector` is the old AFL-shaped policy in new clothes — it treats the frontier as flat.
Task 69 established that discovering novel cells correlates with progress toward bugs. This task
makes selection non-uniform: spend branches where discovery is live, starve regions that have
exhausted. `Selector::choose` picks the next exemplar to branch from; `Selector::reward` feeds
back what the branch produced — that closed loop is the entire surface these policies may use.

## Prior art

- **EcoFuzz** (USENIX Security 2020) [secret] — seed scheduling as a non-stationary/adversarial
  multi-armed bandit, reward = rate of new coverage; the published version of Antithesis's "it
  uses RL". v3's template.
- **AFLFast** (CCS 2016) [eng] — power schedules shifting energy toward low-frequency,
  under-explored regions; v2's count-based weighting is its Go-Explore form.
- **Entropic** (FSE 2020) [eng] — information-theoretic per-seed energy; the named refinement
  path if plain counts prove too blunt.
- **Legion** (ASE 2020) [secret] — MCTS/UCT balancing exploration/exploitation over a state
  tree; the follow-on template if the flat bandit plateaus. Deferred, not built here.
- **AFLGo** (CCS 2017) [eng] — directed energy-by-distance to declared targets; deferred until
  task 73's assertion catalog supplies targets.

## What to build

### v2 — `CountSelector` (Go-Explore count-based)

Weight each frontier cell ∝ 1/√(visits+1), where visits = times this selector chose an exemplar
of that cell (tracked from its own choose/reward history — the `Archive` is never consulted for
meaning). Sample by cumulative weight from the caller-seeded `Prng`. Weights are integer /
fixed-point — the choice is state-affecting, so no floating point (conventions rule 4).

### v3 — `BanditSelector` (non-stationary bandit + STADS stop)

Arms = frontier cells. Reward = the rate of **new cells discovered per branch** over a sliding
window, decaying as a region exhausts — non-stationary by construction (an arm's estimate ages;
recency-weighted mean or an explicit decay factor). The selection policy is an
adversarial/non-stationary MAB (EcoFuzz's adaptive-average shape or EXP3-style — implementer's
choice; pin the chosen policy with tests). The **STADS estimator** (reuse
`dissonance/explorer/src/stads.rs` — task 69's contract; do not reimplement) supplies the
exhaustion signal: per-arm Good–Turing discovery probability de-prioritizes spent regions, and
the campaign-level estimate is the stopping rule — expose `should_stop(&self) -> bool` so a
campaign ends when estimated discovery probability falls below a configured ε. No `f32`/`f64`
anywhere in the crate (mirror task 71's no-float gate): decay/EXP3-style weights and Good–Turing
ε comparisons are cross-multiplied integer rationals (conventions rule 4).

Reward plumbing: consume the spine `Reward` as landed. If it lacks the new-cells-per-branch
count v3 needs, the additive extension is a task-64 API adjustment coordinated through the
foreman — never a parallel reward type forked here.

### Semantics that must hold

1. Both implement `spine::Selector` exactly (names, roles, semantics — hard rule 3).
2. **Determinism discipline:** given `(seed, identical frontier-event + reward history)`, the
   sequence of choices is identical — the fixed `Selector` contract.
3. **Progression blindness:** opaque `CellKey`s and `Reward`s only. The crate imports no fault
   type, no signal channel, no `CellFn`; its dependency surface is `explorer` + the whitelist.

### Tactic-portfolio arm seam (ruling)

This crate **defines** the `Arm` trait and the arm-selection / arm-level-reward interface (hard
rule 2: interfaces live in the consumer — the selection policy consumes arms). Task 72 depends
on this crate and implements the arms. Arm-level reward is routed by the campaign root —
distinct from the spine's exemplar-keyed `Selector::reward` — so no spine change is required.

## Acceptance gates

1. **Standard suite** green on `dissonance/selector-bandit` (build / nextest / clippy
   `-D warnings` / fmt / deny), all-features, macOS + Linux.
2. **Determinism proptests (≥256)** for v2 and v3: identical `(seed, frontier history)` ⇒
   identical choice sequence. Plus non-stationarity units: an arm with a window of
   zero-new-cell rewards loses priority; a fresh-discovery arm gains; `should_stop` flips
   exactly when the estimator crosses ε on a synthetic discovery stream.
3. **Blindness gate:** dependency surface is `explorer` + whitelist only; noted in
   `IMPLEMENTATION.md` and checked in review.
4. **Box gate (frontier; requires task 69 = GO):** on the task-69 benchmark, v2 and v3 **each
   beat the v1 baseline median time-to-seeded-bug** — median branches-to-find over **≥20 seeds
   per selector per bug**, same trigger thresholds and budgets as 69's report, medians +
   variance reported (Klees-style trial discipline). Append the comparison table to
   `dissonance/benchmark/CORRELATION-REPORT.md` or the crate's `IMPLEMENTATION.md`.

## Box-safety (CRITICAL — gate 4 only)

Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock + verified
after every run: `pkill -9 -f` the campaign bin (and any `live_*`) FIRST → wait
`lsmod | grep '^kvm_intel'` users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe kvm_intel` →
verify size 1396736 on a FRESH ssh connection. SSH drops (exit 255) on pkill/rmmod are normal —
reconnect + verify. Pin builds/tests to `taskset -c 2` (`docs/BOX-PINNING.md`). Run gates in the
foreground and READ results before reporting; no detached pollers + idle.

## Non-goals

- MCTS/UCT over the exemplar tree (Legion) — the named follow-on **if the flat bandit
  plateaus**; do not start it here.
- AFLGo-style directed energy toward declared-but-never-hit sometimes-assertions — needs task
  73's catalog; the other named follow-on.
- Any `Archive`, `CellFn`, `Sensor`, or `Oracle` change — a weak signal routes to task 67, not
  to search-side compensation.
- Tactic work (regime faults, PCT, arm implementations, PCT policy) — Phase G, tasks 71/72.
  **Defining the `Arm` interface is in scope here** (the arm-seam ruling above); building arms
  is not.
