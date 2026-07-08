# Task 96 — the campaign stopwatch: hash-neutral phase timing for box runs

> **Delegable, small.** One crate: `dissonance/conductor`. Laptop-gated (portable suite +
> the existing loopback tests); no box gate of its own — the first consumer of the live
> numbers is the next scheduled box run (task 69-M2 reruns, task 95-M2's gate (d), task 86).
>
> **Why:** production box runs are wall-clock-blind. The 2026-07-08 investigation measured a
> live task-69 campaign at **73–80 minutes per 512-branch seed** (~8–9 s per branch against a
> `deadline-delta` whose guest work is sub-millisecond) — and could attribute the time only
> by inference, because no phase in a live run emits a duration: the conductor log has no
> timestamps and no per-branch lines, `CampaignReport` has no timing field, and the only
> wall-clock record of an 11-hour campaign is the shell orchestrator's per-seed START/DONE
> lines. `tasks/60-first-campaign-planted-bug.md` §"Record in IMPLEMENTATION.md" already
> asked for "wall-clock, branches/hour" — never implemented. This task closes that gap with
> observation-only timing in the campaign driver, so per-branch cost decomposes into
> branch/run/hash/harvest on every run, including the before/after evidence for task 95's D5
> work.

Read first: `tasks/00-CONVENTIONS.md` (rule 4 and the clippy escape hatch);
`dissonance/conductor/src/campaign.rs` (`run_campaign`, `CampaignReport`,
`render_campaign_table`); `dissonance/conductor/src/boxrun.rs` (boot-to-marker);
`clippy.toml` (the `Instant::now` ban and its `// not order-observable:` allow pattern);
`tasks/87-film.md` §hash-neutrality (the observation-only precedent this follows);
`consonance/vmm-core/tests/seal_rate_sweep.rs::watchdog_start` (the existing sanctioned
`Instant::now` shape).

## Environment

Touch only `dissonance/conductor/`. All gates laptop-side, macOS + Linux. No new
dependencies. The timing layer must be a pure addition: every existing test passes
(updated only where it deliberately asserts on report rendering — see gate 4).

## The determinism stance (read this before writing any code)

Wall-clock is banned from anything that reaches state, a hash, an encoded reproducer, or a
journal (`clippy.toml`, conventions rule 4; `runtrace` stays wall-clock-free). This task uses
the sanctioned escape hatch: **host-side observation that is read into a report and nowhere
else** — the same category as the film projector ("hash-neutral by construction") and the
live-harness watchdogs. Concretely:

- All `Instant::now` calls live in ONE new module, `dissonance/conductor/src/stopwatch.rs`,
  under a single file-level `#[allow(clippy::disallowed_methods)]` with a
  `// not order-observable:` justification stating: durations are emitted only into
  `CampaignReport.timing` and log lines; they never reach `state_hash`, any `Environment`/
  reproducer, the runtrace journal, or any branch decision.
- Timing values are integers (microseconds, `u64`). No floats stored; a float may appear
  only in a formatted print (e.g. branches/hour with one decimal).
- Nothing in the search loop may *branch* on a duration. The stopwatch records; it never
  decides. (A future time-budgeted campaign knob is out of scope and would need its own
  ruling.)

## What to build

### 1. `stopwatch.rs` — the recorder

A small module, fully unit-testable with injected samples (the `Instant` reads are the only
untestable lines; keep them trivial):

```rust
/// One phase's accumulated observations, microseconds.
pub struct PhaseStats { pub count: u64, pub total_us: u64, pub p50_us: u64, pub p90_us: u64, pub max_us: u64 }

/// Records per-phase durations during one campaign. Observation-only (see module doc).
pub struct Stopwatch { /* per-phase Vec<u64> samples + campaign t0 */ }

impl Stopwatch {
    pub fn new() -> Self;
    /// Time one closure under a phase label; returns the closure's result.
    pub fn time<T>(&mut self, phase: Phase, f: impl FnOnce() -> T) -> T;
    /// Seconds since `new()` (for progress lines).
    pub fn elapsed_secs(&self) -> u64;
    /// Fold samples into per-phase stats (percentiles by sorted index — integer math,
    /// deterministic given the same samples).
    pub fn stats(&self) -> BTreeMap<Phase, PhaseStats>;
}

#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
pub enum Phase { Boot, BaseSeal, Branch, Run, Hash, Harvest, Judge, Replay, Nominal }
```

Percentiles: `sorted[(count-1) * P / 100]` (nearest-rank on the sorted sample vec) — exact,
integer, no interpolation. A phase with zero samples is omitted from `stats()`.

### 2. Wire it through `run_campaign` (`campaign.rs`)

Wrap each `machine.*` call in the existing phases — base-seal retry loop (`BaseSeal`), and
per search iteration: `machine.branch(..)` → `Branch`, `machine.run(..)` → `Run`,
`machine.hash()` → `Hash`, the `sdk_events` round-trip → `Harvest`, `oracle.judge` →
`Judge`; the verify-replay loop's branch+run+hash together → `Replay`; the nominal-control
pass → `Nominal`. The `Stopwatch` is constructed inside `run_campaign` (no signature change
for callers beyond what the report needs) and its stats land in the report.

**Progress line:** every 32 search iterations (a `const`), print one line via the existing
conductor log style:
`[conductor] progress: branch 128/512, elapsed 1042s, avg us — branch 5210000 run 1200 hash 2900000`
(totals/counts from the stopwatch; integer µs). This is the line that makes a silent
80-minute run legible from `tail -f`.

### 3. `CampaignReport.timing` + rendering

Add `timing: BTreeMap<Phase, PhaseStats>` (serde as stable snake_case strings) and
`wall_secs: u64` and `branches_per_hour_x10: u64` (integer, ×10 fixed-point; derived from
`branches_explored` and `wall_secs`; 0 when `wall_secs == 0` — no division panic) to
`CampaignReport`, serialized into the campaign's `--out` JSON. Extend
`render_campaign_table` with a timing section (one row per phase: count, total s, p50/p90/max
ms). The JSON stays additive: every existing field is unchanged, so downstream readers
(benchmark-report, task-69 stats) are unaffected — verify none deserializes with
`deny_unknown_fields`.

### 4. Boot-to-ready in `boxrun.rs`

Time from server boot start to the readiness marker and print it next to the existing
"readiness marker reached at step {steps}" line (extend that line with `, wall {secs}s`);
feed it into the report's `Boot` phase.

## Invariants

1. **Hash-neutrality:** timing never reaches `state_hash`, any reproducer/`Environment`,
   or the runtrace journal. All `Instant` reads confined to `stopwatch.rs`.
2. **Observation, never decision:** no control-flow in campaign/boxrun depends on a
   duration. (The existing behavior with the stopwatch removed is bit-identical in every
   hashed/encoded output.)
3. **Deterministic given samples:** `stats()` and all rendering are pure integer functions
   of the recorded samples (`BTreeMap` ordering, nearest-rank percentiles).
4. **Additive report:** existing `CampaignReport` fields, JSON shapes, and PASS/FAIL
   verdict logic are unchanged.

## Acceptance gates

1. Standard suite green on `dissonance/conductor` (build / nextest / clippy `-D warnings` /
   fmt / deny), macOS + Linux. The clippy allow appears exactly once, in `stopwatch.rs`,
   with the justification comment.
2. Unit tests for `stopwatch.rs`: nearest-rank percentiles on fixed sample vecs (including
   count 1 and count 2 edges), zero-sample omission, `time()` passthrough of the closure's
   return value.
3. **Hash-neutrality regression:** the existing loopback campaign test(s) extended to run
   the same mock/loopback campaign twice and assert every non-`timing`, non-`wall_secs`
   report field (base hash, finds, branches, replays, verdict) is identical across the two
   runs — pinning that timing variance cannot leak into campaign results.
4. Any test that asserts on `render_campaign_table` output or report JSON updated
   deliberately (timing section normalized or excluded from the assertion), with a comment
   saying why.
5. A ≥256-case proptest is NOT required (the logic is percentile math; the unit edges in
   gate 2 suffice) — noted here so review doesn't ask for one.

## Non-goals

- **Server-side per-verb timing in `consonance/vmm-core/control.rs`** — the conductor-side
  wrapper conflates client wait + socket + server work, which is acceptable for the first
  decomposition; a server-side mirror is the named follow-up if the conductor numbers point
  into the socket. Out of surface here.
- Telemetry-console integration, `runtrace` stamps (stays wall-clock-free by contract),
  `scripts/box-window.sh` changes (the shell layer already timestamps).
- Any time-budget/deadline *policy* driven by wall-clock — needs its own ruling.
- Task-95 M2 will *consume* these numbers (its gate (d) timing report should quote the
  stopwatch, not hand-run `time`); nothing from that task lands here.
