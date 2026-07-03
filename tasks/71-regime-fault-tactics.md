# Task 71 — `dissonance/tactics-regime`: bursty regime fault tactics + `EnvCodec` sequence mutators (Phase G1)

> **DELEGABLE · Phase G1, off the critical path.** The first real `Tactic` implementations behind
> the task-64 spine: a Markov calm/storm **regime process** that makes fault entropy bursty and
> autocorrelated — real-world faults cluster, and it is the *burst* (a second fault landing while
> the system is still recovering from the first) that finds recovery-path bugs an IID coin never
> reaches — plus AFLNet-style **sequence mutators** over `EnvCodec` schedules (the mutation axis
> the entropy axis composes with). Pure logic, single crate, Mac-gated. `docs/EXPLORATION.md`
> names G1 explicitly parallelizable: start any time after **task 64** lands (the `Tactic` trait +
> open-loop contract). Enforcement of what these tactics emit is tasks 59/61's business, not yours.

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` ("The Proposal seam: Tactic +
EnvCodec" + the Phase G roadmap row), `docs/DISSONANCE.md` ("The guest fault model", "The two
loops"), `tasks/64-explorer-spine-refactor.md` + `dissonance/explorer/src/spine.rs` (the `Tactic`
trait and the open-loop proptest pattern — your contract), `dissonance/environment/src/`
(`Moment`/`Action`/`HostFault` in `host.rs`, `EnvSpec` in `recorded.rs`, `FaultPolicy` in
`policy.rs`, and `EnvCodec::mutate` in `envcodec.rs` — the schedule-safety precedent your mutators
must match), `tasks/59-host-plane-enforcement.md` (the enforced v1 host-fault vocabulary).

## Environment

Pure-logic, macOS + Linux, laptop-gated: one new crate, `dissonance/tactics-regime/`, branch
`task/tactics-regime`. Per the task-64 plugin pattern ("the plugin crates in later tasks depend on
`explorer` and implement them"), this crate may depend on the workspace crates
`dissonance/explorer` (the spine) and `dissonance/environment` (the vocabulary) — hard rule 2's
"no sibling deps" is waived exactly that far and no further. No box, no `unsafe`, no new external
dependencies beyond the whitelist.

## Context

The explorer's only fault entropy today is IID: `FaultPolicy::sample` flips an independent
per-decision coin. FoundationDB's operational lesson is that real-world faults are bursty and
autocorrelated (Hurst-exponent-correlated clusters), and at any IID rate low enough to let the
workload make progress, a genuine storm essentially never occurs — so the recovery-under-continued-
fire paths go untested. `docs/EXPLORATION.md`'s Proposal seam splits proposal into two axes:
**EnvCodec** pre-populates the `Moment→Action` schedule (the mutation axis, outer) and the
**Tactic** answers residual decisions online from a stateful distribution (the entropy axis,
inner); the recorded union is the reproducer. This task ships the first real content for both
axes: regime-modulated entropy and region-based schedule mutation. Both are one arm of the
eventual portfolio (tasks 70/72), never a replacement for the `quiet`/IID baselines.

## What to build

### 1. `RegimeProcess` — Markov calm/storm modulation

A small-N-state (v1: two-state `Calm`/`Storm`) Markov chain with integer-rational transition
probabilities (`num/den`, the `FaultPolicy` idiom), advanced one step per surfaced
**fault-class** decision. *(Integrator ruling, 2026-07-02: originally "per surfaced decision"
— PR #52 implements per-fault-draw advancement (supply-class decisions decline without
stepping), which makes the stationary-rate gates exact in fault-draw space; the stationary
distribution observed at fault draws is identical either way, only burst fragmentation
differs. Ruled: adopt the implemented semantics.)* Each
state carries a per-class fault table (`FaultPolicy`-shaped: probability + eligible faults) —
storms are dwell periods of elevated, clustered fault probability; geometric dwell times give the
autocorrelation. Expose the exact stationary mean fault rate as a rational
(`stationary_rate() -> (u64, u64)`, closed-form for the two-state chain) so gates can construct an
equal-mean IID baseline. Swizzle-style clustered knobs: a `RegimeParams` drawn once per run from
the seed clusters storm intensity/duration, so distinct runs explore distinct regimes. A
multi-timescale nesting (a slow chain modulating the fast one, for heavier-tailed bursts) is an
optional knob, not required.

### 2. `RegimeTactic` — the spine `Tactic`

Implements task 64's `Tactic`: for a **fault-class** `pt`, `decide(&mut self, pt, rng)`
advances the regime one step then samples the active state's table; supply classes answer
nominally **without advancing** (the regime governs the fault classes only — the 2026-07-02
ruling above). Open-loop by construction: its answer is a function of
`(own state, pt, rng)` and nothing else; no `Sensor`/`Archive`/`RunTrace` type may appear anywhere
in this crate's dependency graph.

### 3. Sequence mutators over `EnvCodec` schedules

AFLNet-style region operators over a `Recorded` env's `BTreeMap<Moment, Action>`, each a pure
deterministic `(env, salt) -> EnvSpec` matching `EnvCodec::mutate`'s shape: **insert** (a fresh
host fault, or a copied region, at free `Moment`s), **delete** (a `Moment` range's host
overrides), **retarget** (rewrite one host `Action`'s payload within the legal vocabulary),
**shift** (translate a region's host overrides by a signed `Moment` delta, order-preserving).
Safety rules inherited from `EnvCodec::mutate` and the task-93 ruling: `Action::Guest` overrides
are preserved verbatim (never removed, relocated, or overwritten — a guest answer is
context-bound); no `StandingFault` is ever introduced; `Moment` arithmetic rejects overflow, never
wraps; inserted host faults are confined to the enforced v1 vocabulary (`CorruptMemory`,
`InjectInterrupt`) — `SkewTime`/`SetClockRate` are deferred by task 59 and must not be emitted.

## Invariants (restate in the crate docs; each is gated)

- **(a) OPEN-LOOP Modulation.** A Tactic never reads Sensor/Archive output mid-run; identical
  `(state, point, rng)` ⇒ identical answer. All adaptation happens between runs, in the
  Progression.
- **(b) Determinism discipline.** Seeded PRNG only; **no floats anywhere** — regime probabilities,
  stationary rates, and every statistical gate below are integer/fixed-point (cross-multiplied
  rationals).
- **(c) Progression untouched.** Adding these tactics/mutators grows the Proposal seam only; no
  explorer-engine change is needed or permitted (the `DISSONANCE.md` invariant).

## Acceptance gates

1. **Standard suite** green on `dissonance/tactics-regime` (build / nextest / clippy `-D warnings`
   / fmt / deny), all-features, macOS + Linux.
2. **Open-loop proptest (≥256, the task-64 pattern):** identical `(state, point, rng)` ⇒ identical
   answer, regardless of anything else the test harness varies.
3. **Statistical gates, all integer/fixed-point (≥256 cases each; no `f32`/`f64` in the crate):**
   (a) seed determinism — same seed ⇒ identical decision sequence; (b) **burstiness above IID** —
   over N-draw sequences at the same exact mean rate (IID coin `p` = `stationary_rate()`), the
   regime sequence's windowed Fano factor (or lag-1 autocorrelation) exceeds the IID baseline's by
   a fixed margin; (c) calibration — the empirical fault rate is within a stated integer tolerance
   of the stationary rate.
4. **Mutator gates (≥256 proptests over arbitrary recorded envs):** determinism (same
   `(env, salt)` ⇒ same output); well-formedness round-trip (guest overrides verbatim, no standing
   faults, no `Moment` overflow/collision, result encodes/decodes cleanly per `environment`'s
   codec); vocabulary confinement (only enforced v1 host faults inserted).
5. **Seam-only diff:** no changes outside `dissonance/tactics-regime/`.

## Prior art

- **FoundationDB** (Strange Loop 2014) [eng] — buggify + swizzle: clustered, correlated fault
  regimes; the recipe this task transplants (bursty, Hurst-correlated faults beat IID for
  recovery-path bugs).
- **AFLNet** (ICST 2020) [eng] — region-based sequence mutation over recorded message exchanges;
  directly reusable as the `Moment→Action` schedule mutators.
- **Coyote** (TACAS 2023) [eng] — no single strategy dominates; this is one arm of the task-70/72
  portfolio, not a replacement for the baselines.

## Non-goals

- Fault **enforcement** — tasks 59 (host plane) and 61 (net vertical) own applying what these
  schedules and tactics emit; this crate never touches `consonance/vmm-core`.
- PCT / schedule entropy — task 72.
- Bandit/portfolio arm selection and reward — tasks 70/72; this crate ships tactics, not policy.
- Live beats-baseline validation — benchmark bug (iv), the partition-duration bug, is owned by
  **task 72's portfolio box gate** (its fault-regime arm wraps this crate's tactic) and
  additionally requires task 61 (standing net faults); this task stays pure-Mac, no box gate.
- `SkewTime`/`SetClockRate` fault classes — deferred by task 59; excluded from the mutator
  vocabulary above.
- Modifying `dissonance/environment`'s `EnvCodec` or the spine traits — consume them; wiring the
  mutators into a campaign's proposal path rides the campaign/selector tasks.
