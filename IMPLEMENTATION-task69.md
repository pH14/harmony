# Task 69 — seeded-bug benchmark + signal→bug correlation harness (GO/NO-GO #2)

## ⚠️ Milestone boundary — the GO/NO-GO is NOT decided in this PR

This is **milestone 1**: it lands the *mechanism* (the portable harness + the
engine records-capture fix + the signal-configured dual-config driver + the guest
payloads) and a **determinism smoke test**. **It does not decide GO/NO-GO #2.** The
actual ruling requires the **milestone-2** full ≥20-seed × 2-config × 3-bug
real-KVM campaign that emits `dissonance/benchmark/CORRELATION-REPORT.md`. Until
that runs, **the Phase-F gate is PENDING** — do not read anything here as a GO.

Status: **milestone-1 deliverables complete and gated** (portable suite green on
mac + on the box toolchain; determinism-twice proven; engine fix proven
determinism-neutral). Milestone 2 is the tracked follow-up.

### What the M1 smoke DOES and does NOT validate (the sharp boundary)

The milestone-1 box smoke validates that the dual-config mechanism **runs and is
deterministic** on the box toolchain (same `(seed, config)` ⇒ bit-identical
discovery-event log, for both configs). It does **NOT** validate the signal
configuration's **discriminating power** — i.e. that the log-template signal
actually steers the search better than the blind baseline on the *real* guest.
That requires the signal config to see real guest console logs, which means
**socket console capture** (`explorer::adapter::SocketMachine` populating
`Machine::console()` from the server-side serial capture). That capture is
deliberately **milestone 2** — it is the real-box campaign path, not M1 mechanism
— and is intentionally **not implemented in M1** (`seam.rs` ships only the
default-empty `console()`; the toy machine overrides it so the mechanism is
exercised portably). So: M1 proves the machinery is correct and deterministic;
M2's socket-console-capture + the ≥20-seed real-KVM campaign proves the signal
discriminates and produces the GO/NO-GO ruling.

### Milestone-1 box smoke evidence (real box, `ssh hetzner`, 2026-07-05)

Worktree `~/harmony-t69` at HEAD, built + tested **pinned to core 2** (`taskset -c
2`), box `rustc 1.96.0`:

- `cargo test -p explorer stads` → **10 passed**; `-p benchmark` → **29 passed**;
  `-p conductor --lib benchcampaign` → **9 passed** incl.
  `dual_config_runs_and_is_deterministic_twice` (the box determinism smoke: same
  `(seed, config)` ⇒ identical discovery-event log, for both configs),
  `replays_must_match_the_finding_hash`, `unmarked_crash_is_not_a_find`,
  `terminal_marker_excluded_from_cells`, and `cell_ids_are_content_stable_across_logs`.
  (Counts as of the round-6 head.)
- **KVM untouched** (only cargo build/test — no patched module loaded); verified
  stock **1396736** before and after. No `box-window` lease needed.

**Real-KVM campaign is blocked for milestone 1 by two things, both milestone-2
scope:** (a) the socket-console capture the signal config needs (the driver reads
`Machine::console()`; the socket machine does not yet capture guest serial → the
engine fix gives it a home, wiring it is M2); (b) a **pre-existing** bin-build
break — `conductor/src/main.rs:674` has a non-exhaustive match on
`vmm_core::vmm::Step` (`SdkStop` not covered) that the box's newer `rustc 1.96`
rejects (local `1.94` does not). It is **not** task-69 code (untouched task-58-era
main), but it blocks building the `conductor` *binary* on the box, so the existing
`conductor campaign box` path can't be driven under 1.96 without that one-line fix.
Flagged for the integrator; out of this task's surface.

---

Original handoff notes (pre-ruling) follow.

## What is done (portable, load-bearing, gated on macOS)

Two deliverables, both green under `build` / `nextest` / `clippy -D warnings` /
`fmt` / `cargo deny`:

### 1. `dissonance/explorer/src/stads.rs` — the STADS estimators (the task-70 contract)

A **progression-blind** fold over an opaque `CellKey` discovery-event stream
(`SpeciesAccumulator`), producing Good–Turing discovery probability (`f1/n`),
Chao1 richness (`S_obs + f1²/2f2`), and the species-accumulation curve. **No float
appears in the estimator** — everything is exact `Frac` (reduced rational, `Ord`
by cross-multiplication) and the stopping rule (`discovery_below`) is an integer
cross-multiply. This is the location `tasks/70-selector-bandit.md` names as the
contract its Selector v3 consumes for its state-affecting stopping rule; it is
placed and exported (`pub mod stads`) exactly there.

- 10 unit tests (known-answer Good–Turing/Chao1, spectrum-transition tracking,
  cross-multiplied stopping rule) + 2 proptests (≥512 cases: invariants over
  synthetic communities of **known richness**, and Good–Turing = independent
  `f1/n` count). Order-independence (determinism) is proptested.

### 2. `dissonance/benchmark/` — the manifest + correlation harness

- **`manifest.rs`** — the 3-bug fixture of distinct classes (fault-timing,
  ordering-interrupt, rare-entropy), each with a **tunable** trigger threshold
  dialled so naïve time-to-find sits in 10²–10³ branches, a **distinct per-bug
  serial marker** (`CAMPAIGN_BUG` / `ORDER_BUG` / `UUID_BUG`), and a `CrashKind`.
  `BugClass` and `TriggerParams` are `#[non_exhaustive]` so bugs (iv)
  partition-duration, (v) depth-2 concurrency, (vi) convergence/liveness (tasks
  72/75) slot in **without restructuring** — a new class is a new variant + a new
  trigger predicate, nothing else.
- **`trigger.rs`** — the **toy trigger predicates**, the portable stand-in for the
  guest payloads (bug 1 mirrors `conductor::planted::Trigger` verbatim). Gate 1:
  `triggering_scenario` fires 100% (×25) and a nominal scenario never fires, for
  every bug; near-misses are inert; the rare-entropy fire rate matches
  `2^-prefix_bits`. The entropy draw is a fixed `splitmix64` (no randomness).
- **`stats.rs`** — **the load-bearing math.** Spearman ρ = Pearson on
  tie-corrected midranks (×2-scaled integers), held as `(cov, dx, dy)`; every
  *decision* (`cmp_rho`, effect-size threshold) is an exact `i128`
  squaring/cross-multiplication — **no `f64` in any decision**, only in
  `rho_f64()` for prose. Validated against textbook + `scipy.stats.spearmanr`
  known answers with **pinned exact fractions** (ρ = 4/5 no-ties, 5/6 with-ties,
  ±1 perfect). Median/IQR are exact rationals. A proptest asserts the integer
  `cmp_rho` agrees with the float ρ away from the boundary.
- **`report.rs`** — the four spec measures over the discovery-event logs
  (1: novelty↔progress Spearman per bug; 2: trajectory-through-novelty vs base
  rate; 3: STADS species curves + Good–Turing + Chao1 + prototype stopping rule;
  4: median TTB + IQR, signal vs baseline), and the explicit **GO / NO-GO** ruling
  rendered into `CORRELATION-REPORT.md`. The ruling logic is exact: **GO** iff
  novelty correlates with progress (right direction, ρ ≤ −effect_floor) on ≥2 of 3
  bugs **and** signal median ≤ baseline median on every bug; else **NO-GO** (→
  iterate the CellFn, task 67 — *the search is not the fix*).
- **`src/bin/report.rs`** — `benchmark-report` CLI: campaign-log JSON →
  `CORRELATION-REPORT.md`. Exercised end-to-end on synthetic GO/NO-GO-shaped logs.

The discovery-event log schema the campaign driver must emit is
`report::CampaignLog` (per-branch `touched: Vec<u64>` opaque cell ids + per-bug
`FindRecord { branch, path_len, novel_on_path }`). Cumulative distinct cells and
the STADS stream are derived from it.

## What remains (the box gate) — and two rulings it needs

The box gate (spec gates 2–4) is **not** started. It needs:

- **Guest payloads for bugs 2 & 3** in `guest/linux/` beside task 60's
  `campaign-super.c` / `campaign-init.sh` (unambiguous — `guest/` is in the
  surface list; parallelizable per the spec). Bug 2 = a handler that corrupts
  shared bookkeeping if an `InjectInterrupt` lands mid-update; bug 3 = a
  `gen_random_uuid()`-prefix branch that poisons state. Each with its distinct
  serial marker and the terminal convention `campaign-init.sh` already documents.
- **The campaign driver** that runs both configurations and emits `CampaignLog`s.

> **RULINGS (integrator, this session):** #1 → **build in `dissonance/conductor`**
> (extend the real task-60 bin; no duplicate vmm-core bin). #2 → **YES, task 69
> owns the signal-configured driver** — a signal→bug correlation is vacuous
> without a signal config, and the components (Archive/CellFn v1/Selector/
> materialization, tasks 64/66/67/68) are all merged, so this is integration
> wiring, not from-scratch. The box GO/NO-GO campaign is run **in this session**
> over ssh (not handed off). Surface list amended to include `dissonance/conductor`.

### RULING (was #1) — driver location (hard-rule-1 boundary)

The spec's surface list names **`consonance/vmm-core`** to "extend the task-60
campaign bin". **That bin does not live there** — task 60 landed the campaign in
**`dissonance/conductor`** (`conductor/src/campaign.rs`; the `conductor` bin).
`vmm-core` has no campaign bin. Extending the task-60 campaign therefore means
touching `conductor`, which is **not** in this task's named surface list. Need a
ruling: (a) touch `conductor` (amend the surface list to match reality), or
(b) build a new benchmark-campaign bin under `vmm-core` as the spec literally
says. Recommend (a) — extend `conductor`, the real task-60 bin.

### RULING NEEDED #2 — "signal configuration" scope

The correlation harness compares **signal** (the Phase-D stack: RunTraces →
sensors + CellFn v1 → Archive + default v1 Selector → materialization) against
**baseline** (blind seed search). **No existing driver runs the signal loop
against a real `Machine`.** The task-60 conductor campaign is blind-seed only
(that is the *baseline*); nothing today drives `Explorer::explore` /
`progression_step` (Archive/CellFn/Selector/materialize) against the socket
`Machine`. Building that signal-configured driver — and emitting per-branch
`new_cells` discovery events from it — is the substantive box half of this task.
Confirm task 69 owns building it (vs. it being assumed to already exist / being
task-70 territory). It is buildable from the landed 64/67/68 spine, but it is real
integration work, not a thin extension of the task-60 bin.

### The signal-driver blocker (discovered while wiring)

The signal cells come from **task-67 `logtmpl`** (`LogSensor` + `CellFnV1`), which
scrapes **`RunTrace.records`** (guest console log lines) — *not* a hardware
coverage bitmap (the toy exposes none; the socket machine's is "zero-width").
**But neither driving loop populates `records`:** `conductor::campaign::trace_of`
hard-codes `records: Vec::new()` (`campaign.rs:409`), and
`Explorer::progression_step` likewise builds its trace with `records: Vec::new()`
(`engine.rs:435`). Only the standalone recording loop (`conductor::record::
run_recording`) captures console → `records` (via `runtrace::decode_chunks`).

Consequence: a `LogSensor` wired into the existing campaign or into `Explorer`
sees **zero records and emits zero features → the signal has no cells.** So the
signal-configured driver cannot reuse `run_campaign`/`Explorer` as-is; it must be
a **bespoke search driver** that, per branch: runs the machine, **captures the
guest console into `RunTrace.records`** (as `record.rs` does), runs
`LogSensor::observe` → features → `CellFnV1::key` → the per-branch cell set,
admits to a cell-keyed `Archive`, selects the next base (frontier vs genesis =
signal vs baseline), and judges with the oracle — emitting a
`benchmark::report::CampaignLog` per `(config, seed)`. This is the substantive
integration this task owns (ruling #2), and it is larger than "extend the task-60
bin": it is a new record-capturing dual-config search loop. The toy path needs a
record-emitting toy machine (whose console lines vary with proximity to the
trigger, so cells genuinely correlate with progress) to gate it portably; the box
path swaps in the socket machine + real guest images.

### Box run plan (once the driver + images are built)

1. Build 3 campaign images (bugs 1/2/3) via `guest/linux/build-campaign-image.sh`
   analogues; distinct serial markers.
2. Per bug × {signal, baseline} × **≥20 seeds**: run on patched KVM through
   `box-window.sh`, pinned to a dedicated core (`docs/BOX-PINNING.md`), emitting
   `CampaignLog` JSON. Verify each find replays **25/25** (identical `state_hash`
   at the terminal stop); a nominal-seed control crashes on none; per-bug markers
   attribute every find.
3. **ALWAYS revert KVM to stock 1396736 and verify on a fresh ssh** after every
   patched run (SSH exit-255 on pkill/rmmod is normal — reconnect + verify).
4. `benchmark-report --logs all.json --out dissonance/benchmark/CORRELATION-REPORT.md`;
   commit it. The rendered ruling is the GO/NO-GO handed to the foreman as the
   gate on Phase F.

Box reachability was confirmed this session (`ssh hetzner` OK, kvm on stock,
users=0) but the full campaign is a multi-hour run not completed here.

### Milestone-2 prerequisites (deferred from M1 — the review is tracking these)

The following are **intentionally not implemented in M1** (they are the real-box
campaign path, not the M1 mechanism); each is a prerequisite for the M2 GO/NO-GO
campaign:

1. **Socket console capture** — `explorer::adapter::SocketMachine` must populate
   `Machine::console()` from the server-side serial capture (the `conductor::
   record::run_recording` path already captures console via
   `runtrace::decode_chunks`). Until then the signal config sees no real guest logs
   on the box, so M1 validates the mechanism's determinism but not the signal's
   discriminating power (see "the sharp boundary" above).
2. **Real `logtmpl` cells** — swap the M1 toy's content-keyed line templates
   (`benchcampaign::cells_of`) for the real task-67 `LogSensor` + `CellFnV1` over
   the captured console, with a **persisted campaign codebook** (`codebook_bytes`
   / `with_codebook_bytes`) so cell ids stay stable across the independent logs the
   report pools.
3. **Fault-moment rebasing on the SocketMachine path** — `benchcampaign::
   mint_scenario_env` (≈ line 88) offsets a fault's `Moment` from a fixed base;
   against the real backend the fault Moment must be rebased onto the sealed base's
   V-time (the exact-arrival window `conductor::planted` documents). The **toy path
   does not hit this** (it decodes the scenario directly), so it is **not fixed in
   M1** — it is a SocketMachine-only concern for the M2 driver. *(Flagged by the
   round-6 review; tracked in the M2 follow-up.)*
4. **Three real guest images** (`campaign-super.c` + `order-super.c` +
   `uuid-super.c`) built and validated on the box, per the run plan above.

## Deviations considered & rejected

- **Depending on `environment::HostFault` in the benchmark's trigger model** —
  rejected; the toy predicates are self-contained (their own `FaultKind`) so the
  correlation ground truth is decoupled and purely testable, exactly as
  `conductor::planted` is a decoupled stand-in. The manifest documents the mapping
  to the real task-59 fault vocabulary.
- **A stats crate for Spearman** — rejected (spec: hand-rolled, whitelist-only).
  Hand-rolled integer rank math is also the only float-free way to keep the
  *decision* exact.
- **Incidence (Chao2) vs abundance (Chao1)** — the spec says Good–Turing `f1/n` +
  Chao1; implemented the textbook abundance Chao1 over the discovery-event stream
  (each discovery = one individual), matching the spec's wording exactly.

## Known limitations

- The box gate and the committed `CORRELATION-REPORT.md` are pending (rulings +
  run). The report *generator* and its ruling logic are complete and tested; only
  real campaign logs are missing.
- STADS uses a pooled accumulator across a configuration's seeds (fold in seed
  order). Per-seed curves are available (`SpeciesAccumulator` per log) if the
  report later wants medians-of-curves rather than a pooled curve.
