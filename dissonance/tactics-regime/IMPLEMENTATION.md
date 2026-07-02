# Task 71 — `dissonance/tactics-regime`: implementation notes

The first real content behind the task-64 spine: a bursty calm/storm **regime
fault tactic** (the entropy axis) and AFLNet-style **sequence mutators** over
`EnvCodec` schedules (the mutation axis). Pure logic, one crate, Mac-gated.

## What shipped

- `regime.rs` — `RegimeProcess`, the two-state (`Calm`/`Storm`) Markov chain with
  integer-rational `num/den` transition probabilities; `RegimeParams` (the
  swizzle knob drawn once per run from the seed, always meaningfully bursty);
  `StateTable` (a `FaultPolicy`-shaped per-state, per-class fault table);
  `stationary_rate() -> (u64, u64)`, the exact closed form for the two-state
  chain, reduced to lowest terms.
- `tactic.rs` — `RegimeTactic`, the spine `Tactic`. Advances the regime one step
  on a governed (fault-class) decision, then samples the active state's table;
  supply classes and unidentifiable points decline to the seed. `class_of` /
  `class_tag` expose the `ctx` convention (see below).
- `mutators.rs` — `SeqMutators::{insert, delete, retarget, shift}` (plus a
  salt-dispatched `mutate`), each a pure `(env, salt) -> EnvSpec`.

All acceptance gates pass on macOS: build / nextest (22 tests, incl. the four
≥256-case proptest gates) / clippy `-D warnings` / fmt / `cargo deny`. No
`unsafe` (crate is `#![forbid(unsafe_code)]`), so no Miri bar. No floats
anywhere; every probability and every statistical gate is integer/fixed-point
cross-multiplied rationals.

## Key design decisions

### The `ctx` class convention (the one real interface question)

The spine's `DecisionPoint` is deliberately opaque (`at`, `id`, `ctx: Vec<u8>`),
but the tactic must know a decision's **class** to honor "supply classes answer
nominally — the regime governs the fault classes only." `environment`'s
`DecisionPoint` has no wire codec (it is "a live decision a service reads, never
a serialized blob"), and `DecisionClass::as_u16`/`from_u16` are crate-private.

So this crate defines its own local convention (conventions rule 2): a surfaced
point's `ctx` **begins** with the little-endian `DecisionClass` discriminant.
`class_tag`/`class_of` are the exposed encode/decode of that tag (the
discriminants are restated locally to match `environment`'s stable `#[repr(u16)]`
numbering). A `ctx` too short or carrying an unknown tag makes the tactic
**decline** (empty answer → the seed answers) rather than fabricate — and it
draws no PRNG in that case, so an unrecognized point can never desync a replay.
Wiring a real machine to stamp this tag is the campaign/integration task's job
(a non-goal here); the gates stamp it directly.

### Nominal vs. decline, and when the regime steps

- A **fault-class** decision: `decide` steps the regime and emits an explicit
  encoded `environment::Answer` — `Fault(..)` on a fault draw, `Nominal`
  otherwise. The regime *fully governs* the fault decision (it does not fall
  through to the seed's own `FaultPolicy`), which is what makes the empirical
  fault rate match `stationary_rate()` exactly.
- A **supply**-class (or unidentifiable) decision: the empty answer, so the
  seeded base supplies the value; the regime does **not** advance and draws no
  PRNG. This ties every regime step to an actual governed fault draw and keeps
  the stationary-rate accounting exact.

### Statistical gates: which statistic

- **Burstiness (b)** uses the **windowed Fano factor** (count variance / count
  mean), compared against an equal-mean IID coin at exactly `stationary_rate()`.
  Fano is a *ratio*, so the comparison is a clean integer cross-multiplication
  with no float; it is far more numerically stable than an un-normalized lag-1
  autocovariance (whose sampling fluctuation scales with `n²·m(1−m)` and would
  make a fixed absolute margin flaky). The gate asserts
  `regime_Fano ≥ IID_Fano + 1/2`.
- `RegimeParams::from_seed` is tuned so **every** draw is genuinely bursty
  (storm intensity ≥ 1/2, storm dwell ≥ 8 steps, calm intensity a tiny floor),
  which is what makes (b) robust across all 256 cases. This is deliberate: this
  crate is *the bursty arm*. The `quiet`/IID baseline is a different tactic
  (task 70/72's portfolio) — never emulated here.
- **Calibration (c)** asserts the empirical rate over 40 000 draws is within 5%
  of `stationary_rate()` (cross-multiplied, `i128`).

### Mutator safety

`insert`/`retarget` only ever emit **enforced v1** host faults
(`CorruptMemory`/`InjectInterrupt`); the task-59-deferred
`SkewTime`/`SetClockRate` are never produced, and `insert`'s region-copy path
**sanitizes** a non-v1 source to v1 so copying can't smuggle a deferred fault in.
`delete`/`shift` only remove/relocate, never introduce. Guest overrides are
preserved verbatim, standing faults are carried through untouched (never added),
and `shift` uses **checked** `Moment` arithmetic — an overflow or a collision
with a retained (guest or out-of-region host) override **fails closed** (the env
is returned unchanged), so no guest is ever clobbered and no two overrides ever
collapse onto one `Moment`.

`free_slot` (draw a word, scan upward with `wrapping_add` past **any** occupant)
guarantees `insert`'s destination is genuinely free, so it adds exactly one
override and never replaces an incumbent. This is deliberately **stricter** than
`environment::EnvCodec`'s `free_non_guest_slot`, which tolerates overwriting a
host action — a same-`Moment` host replacement is the silent-drop class the
task-59 ruling-B outlawed, so the region-scoped mutators here reject it (round-1
review fix). The wrap is a *slot search*, not a `Moment` translation — no
override's key is ever arithmetic-wrapped, satisfying "reject overflow, never
wrap" for the translation paths that matter.

`stationary_rate()` special-cases a **frozen chain** (`P(C→S) == P(S→C) == 0`,
which `new` accepts as a valid degenerate "never leaves Calm" regime): the
`p+q` denominator is zero, so it returns the **calm** table's probability — the
exact long-run rate the frozen process exhibits from its Calm start state —
rather than a degenerate `0` that would hand the statistical gates a wrong
baseline (round-1 review fix).

## Deviations considered and rejected

- **Reusing `environment::FaultPolicy` for the per-state table.** Its
  `sample`/`as_u16` are crate-private, so this crate reimplements the identical
  `w % den < num` Bernoulli idiom in `StateTable` ("`FaultPolicy`-shaped", per
  the spec) rather than depending on private API.
- **Deriving `serde` on the regime types.** `environment::Fault` is not `serde`,
  and no gate needs serialization, so serde was left off to keep the dependency
  surface minimal. Add it (with a manual `Fault` codec) if a later reproducer
  wants to persist a drawn `RegimeParams`.
- **Un-normalized lag-1 autocorrelation for gate (b).** Rejected for the
  flakiness reason above; the Fano ratio is the stabler integer discriminator.

## Known limitations / notes for the integrator

- **`DEN_CAP = 4096`.** `RegimeParams::new` caps every probability denominator at
  `2^12` so the `stationary_rate` closed form provably fits `(u64, u64)` after
  reduction. 4096-step probability granularity is far finer than the calm/storm
  contrast needs; raise `DEN_CAP` only alongside re-checking the `u128` overflow
  budget in `stationary_rate`.
- **Multi-timescale nesting** (a slow chain modulating the fast one) is the
  spec's optional knob and is **not** implemented; the two-state chain is the v1.
  The `RegimeParams` shape leaves room to add an outer chain later.
- **Non-goals honored:** no enforcement (never touches `consonance/vmm-core`), no
  PCT/schedule-entropy, no bandit/portfolio arm selection, no live
  beats-baseline box gate (task 72 owns that), no `EnvCodec`/spine changes — the
  crate consumes both. Wiring these mutators into a campaign's proposal path
  rides the campaign/selector tasks.
