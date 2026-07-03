# Task 67 — `dissonance/logtmpl`: the log-template scrape sensor + CellFn v1

> **DELEGABLE · pure-Mac · fixture-driven.** The first real signal channel: Drain-style
> log-template clustering turns the open-vocabulary console-log stream (the scrape tier —
> "the primary channel", `docs/EXPLORATION.md`) into stable `Feature`s via a codebook **internal
> to this crate**, adapts log records to the spine `Matchable` trait so task 66's DSL can match
> on templates and parameters, and ships **CellFn v1** — the first multi-channel point-in-time
> cell function. This is the other half of `docs/EXPLORATION.md` Phase D (task 66 is the DSL).
>
> Depends on **task 64** (the spine in `dissonance/explorer/src/spine.rs`) having merged —
> dispatch only after 64 merges (the crate cannot compile without `spine.rs`); until then this
> spec is contract-only; never redefine spine items, import them. Consumes task 66 only *through*
> the spine `Matchable` trait (**no crate dependency on `dissonance/matcher`**) and task 65 only
> through the spine's scrape-tier `Record` type plus its fixture drops if they exist.
> Parallel-safe with 65/66/68.

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` ("The Scoring seam, elaborated" —
the codebook fold and the coverage-is-terminal ruling; "The three signal tiers"; "The two hard
problems" — the cell abstraction is the whole game and it is isolated in `CellFn` on purpose),
`tasks/64-explorer-spine-refactor.md` (`RunTrace`, `Feature`, `FeatureSet`, `CellKey`, `Sensor`,
`CellFn`, `Matchable`), `tasks/66-matcher-dsl.md` (the DSL this crate's adapter serves; the
`cell`-role channels CellFn v1 composes), `tasks/65-runtrace-recorder.md` if written (scrape-tier
`Record` decode + fixture provenance), `docs/DISSONANCE.md` ("The two loops" — post-task-94 read
Theme → **Progression**, Variation → **Modulation**).

## Environment

Pure-logic, macOS+Linux, laptop-gated; single crate `dissonance/logtmpl` (hard rule 1). One
sanctioned sibling dependency: **`dissonance/explorer`** (the spine — rule 2 is satisfied in the
consumer); no other sibling. Environment vocabulary (`Moment`, `Environment`) arrives via
`explorer`'s spine re-exports; if the landed spine does not re-export them, `dissonance/environment`
is granted exactly per task 71's waiver. Whitelist deps only: `serde`+`serde_json` (codebook
serialization), `sha2`, `thiserror`, `proptest`. **Committed fixtures** under
`dissonance/logtmpl/tests/fixtures/` make every gate Mac-runnable — no box, ever, for this task.

## Context

Off-the-shelf software (Postgres, k3s) tells you what state it is in — on its console. But log
lines are open-vocabulary: raw text can't be a `FeatureId`. The standard fix is log-template
clustering (Drain): strip parameters, cluster lines into template *species*, and the species
stream becomes a stable, low-cardinality signal. Per the EXPLORATION ruling, the codebook that
stabilizes an open vocabulary is **internal to the plugin that needs it** — stable `FeatureId`s
cross this crate's boundary; template text, tree structure, and clustering thresholds never do.
The spine and explorer never learn clustering exists. Downstream, this crate also owns the first
serious answer to hard problem #1: **CellFn v1**, composing the template-species channel with the
matcher's `cell`-role channels into a bounded `CellKey` — the mandatory bound on archive size,
best-per-cell domination assumed from day one (a long k3s run hits thousands of `Moment`s).

## Template clustering (Drain-style)

Inputs are the scrape-tier records of a `RunTrace` (`t.records`, log-kind entries); tests feed
them from fixtures via a thin in-crate loader (line index → synthetic `Moment`). Algorithm — a
deterministic fold, all integer math:

- Tokenize on whitespace. Pre-mask tokens containing digits to `<*>` (a knob, default on).
- Fixed-depth parse tree: bucket by token count, then by the first `D` tokens (default `D = 2`),
  reaching a leaf list of candidate templates. The tree keys the first `D` tokens by **kind**
  (literal-vs-wildcard), not their display text, so a literal token that is textually `<*>` never
  routes into the same leaf as a masked wildcard.
- Similarity scores the template's **constant (non-`<*>`) positions only**: `matches / constants`
  where `constants` is the number of literal positions in the template and `matches` is how many
  equal the line's token; wildcard positions are excluded from **both** numerator and denominator.
  Compared against a threshold `τ = num/den` by cross-multiplication, **strictly above** τ —
  **no floats anywhere** (hard rule 4). A **zero-constant template (all `<*>`) matches nothing** via
  this rule. Strictly above `τ`: merge into the best candidate (most matched constants; ties break
  to the lowest existing template id), with differing positions generalized to `<*>`. Not above `τ`
  for any candidate: **new template — unless the leaf already holds a live template with this exact
  shape**, in which case reuse it (a zero-constant line, e.g. a blank or all-digit line, cannot
  match via similarity yet must still get a stable, non-duplicated id — the shape-uniqueness
  invariant applies on the mint path too).

  > **Amendment (foreman-authored, shipped with PR #53).** The original bullet — "count of
  > *exactly-equal token positions* [over all positions], at or above τ" — is self-inconsistent
  > once `<*>` exists: scoring wildcards as mismatches remints already-absorbed lines after a
  > template generalizes (the round-3 reproduced id-instability: `[0,0,0,0,1] → [0,2,2,0,1]`),
  > while scoring wildcards as free matches over-merges distinct species (the round-5 example:
  > `a b y q r` scores `3/5 ≥ τ` against `a b <*> d e` and wrongly merges). Scoring the constant
  > positions only is the unique local rule satisfying both: an absorbed line still matches every
  > surviving constant (`constants/constants = 1`, stable ids), while `a b y q r` shares only the
  > `a b` prefix (`2/4`, not above τ → a new species, no over-merge).

## The codebook — internal, serialized, stable

`template → FeatureId` in first-seen order (a stateful fold over the run *sequence*, not just one
run): a `BTreeMap`-backed structure with a version field, serialized deterministically via
`serde_json` (BTree ordering; no map with unstable iteration anywhere near the encoder).
Serialize → reload → continue must be indistinguishable from never having stopped. The Sensor
emits `Feature { channel: templates, id }` with ids already stabilized; nothing codebook-shaped
appears in any public signature that the spine or another crate could couple to.

**Shape-uniqueness invariant + id aliasing (integrator ruling, Option A).** No two *live*
templates may share the same shape. When a merge-generalization would make a template's shape
equal an existing live template's, the two instead **merge into the survivor** (the lowest id):
the other id is retired (removed from its parse-tree leaf) and an entry `retired_id → survivor_id`
is recorded in a serialized **alias table** (a `BTreeMap`, deterministic; the codebook's serialized
state gains this field, so the version is bumped). Every id the crate returns — `Feature` emission,
the `Matchable` adapter, CellFn's folded ids — is **canonicalized** through the alias table
(survivors are always lower, so canonicalization strictly descends and cannot loop). A historical id
therefore stays meaningful even after two species converge; exact re-derivation of a recorded trace
goes through its recording-time snapshot (the re-derivation contract below).

> **Amendment (integrator ruling, shipped with PR #53).** Without this invariant, convergent
> generalization can make two template shapes identical (`a b c d e` → 0, `a b x y z` → 1, one-token
> variants generalize both to `a b <*> <*> <*>`); a re-arriving `a b x y z` then scores `2/2` on
> both and the lowest-id tie-break silently reassigns it across species, so previously-emitted id-1
> features disagree with later re-derivation. This is the third manifestation of one root cause —
> *stateless re-scoring against an evolving template set cannot guarantee assignment stability* (see
> also the round-3 wildcard-scoring and round-5 adapt/observe-ordering fixes) — so it is fixed
> structurally (shape-uniqueness + aliasing), not by another point patch. Options B (per-line memo,
> unbounded) and C (freeze-on-first-assignment, weakens cross-run identity) were considered and
> rejected.

**Re-derivation contract (integrator ruling D1; `INTEGRATION.md` 6c).** "Serialize → reload →
continue is indistinguishable" is the *persistence* guarantee (a serialized codebook resumes
bit-identically). **Exact re-derivation of a recorded trace** is defined as **replay against the
codebook snapshot as of recording time** — the task-65 runtrace store already persists that
snapshot, so a recorded trace re-derives bit-for-bit by construction. What is **not** guaranteed,
and is **explicitly accepted as documented clustering drift** ("canonical modulo drift"), is
re-observing a trace against a *later, evolved* codebook: shape convergence is still reconciled by
the Option-A alias table, but a **cross-observe erosion-steal** — a line reassigned between two
still-live species because a *later* line eroded a constant the tie-break had relied on — can shift
a line's canonical species with no reconciling alias (the two templates never converge to one
shape). This is a 4th manifestation of the same *stateless-re-scoring* root cause; ruling D1 accepts
it rather than complicating the tie-break, because the recording-time snapshot already gives exact
replay where exactness is required. (The alternative directions D2/D3 — reshaping the tie-break or
aliasing on every steal — were considered and rejected; see `INTEGRATION.md` 6c for the full
rationale.)

## The `Matchable` adapter

A `TemplateRecord` (log record + its assigned template) implementing spine `Matchable`:
`kind() = "log"`, `attr("msg")` = the raw line, `attr("template")` = the template id,
`attr("param.N")` = the Nth extracted parameter, `moment()` = the record's `Moment`. That is the
full contract with task 66 — its DSL then matches `{ "kind": "log", "attr": { "msg": "database
system is ready*" } }` or on `template`/`param.N` with no dependency between the two crates.

## CellFn v1 — multi-channel, point-in-time, bounded

`CellFn::key(at, feats)` composes, in fixed channel order, a length-prefixed byte encoding of:

1. **species-progress** — log2 bucket of the count of distinct template species seen at ≤ `at`;
2. **last-new-species** — the `FeatureId` of the most recently first-seen template, folded
   `mod k` (default `k = 64`);
3. **each matcher `cell`-role channel** — the latest value-id observed on that channel at ≤ `at`
   (the reified state SGFuzz says to harvest: pod phase, recovery state), folded `mod k`.

**Coverage is excluded by construction** (the EXPLORATION ruling: coverage is a TERMINAL signal —
it feeds terminal admission and is never blended into along-timeline cell keys). CellFn v1 takes
no coverage input; do not add one. **Cardinality-control knobs are mandatory, not optional**:
per-channel enable, quantization (log2 vs identity for counters), and fold modulus `k`, all in a
serde config with the defaults above. The cell function is the archive's only size bound — too
fine explodes the archive on a single trajectory, too coarse makes progress invisible (hard
problem #1); the knobs are what task 69's correlation harness will tune, so they must be
config-visible, not constants.

## Fixtures

Commit `tests/fixtures/postgres-console.log` and `tests/fixtures/k3s-console.log`. Source them
from task-65 fixture drops if present (serial-console captures under
`dissonance/*/tests/fixtures/` or `consonance/vmm-core/tests/`); otherwise synthesize
representative ones: Postgres startup/WAL/checkpoint/autovacuum lines and k3s
kubelet/containerd/flannel/etcd lines with realistic parameter churn (pids, IPs, durations,
UUIDs). The k3s fixture must be ≥ 5,000 lines (the cardinality gate needs it); keep each under ~2 MB.

## Semantics that must hold

1. **Codebook internality** (the EXPLORATION ruling): stable ids out, clustering internals never.
2. **Determinism:** clustering is a deterministic fold — identical input stream ⇒ identical
   species set, identical `FeatureId`s, byte-identical serialized codebook; integer math only;
   no `HashMap`/`HashSet` order reaches any output (clippy.toml enforces; structure the code so
   there's nothing to `#[allow]`).
3. **Coverage stays terminal:** no coverage-derived value ever enters a `CellKey`.
4. **The cell function bounds the archive:** default knobs must pass gate 5; unbounded keys are a
   spec violation, not a tuning matter.
5. **Progression blindness:** `CellKey` is opaque bytes to everything downstream; this crate
   implements spine traits and touches nothing else in `dissonance/explorer`.

## Prior art (design anchors, not a bibliography)

- **Mallory** (Meng et al., CCS 2023) [beyond] — greybox fuzzing of real distributed systems on
  whole-system log/event novelty with zero source instrumentation; proof the scrape tier alone
  carries enough signal to guide search on off-the-shelf software.
- **SGFuzz** (Ba et al., USENIX Security 2022) [secret] — harvest state the system already
  reifies rather than inventing abstractions, and treat sequences (n-grams over value traces) as
  features; CellFn v1's channels carry reified values, and n-gram channel variants are the
  natural v2 knob.
- **ModelFuzz** (Nagendra et al., OOPSLA 2025) [secret] — a small formal model supplies the cell
  abstraction for a specific protocol; that is the named follow-on for protocol-specific
  CellFns, explicitly not built here.

## Acceptance gates

1. **Standard suite** green on `dissonance/logtmpl` (build / nextest / clippy `-D warnings` /
   fmt / deny), all-features, macOS + Linux.
2. **Stable species set:** two independent derivations (fresh codebook each) over each committed
   fixture yield the identical species set, identical `FeatureId` assignment, and byte-identical
   serialized codebooks.
3. **Codebook reload:** serialize mid-fixture, reload, finish — species set, `FeatureId`s, and
   final serialized bytes identical to the uninterrupted run.
4. **Proptests (≥256):** every line clusters (totality, no panic on arbitrary bytes); lines
   differing only in masked parameter positions land in the same template; codebook round-trip;
   `CellKey` encoding is injective over distinct channel-value tuples and stable under re-encoding.
5. **Cardinality bound:** with default knobs, the number of distinct `CellKey`s over the full
   k3s fixture timeline is ≥ 32 and ≤ 1,024, asserted in a test (not degenerate, not exploding).
6. **Adapter unit tests:** for known fixture lines, the `Matchable` impl exposes the documented
   `kind`/`msg`/`template`/`param.N`/`moment` values.

## Non-goals

- OTel spans (task 74), SDK/link-tier events (task 73), and raw console → `Record` decoding
  (task 65 — this crate consumes decoded records; the fixture loader is test scaffolding, not a
  decoder).
- A k8s-events channel plugin — a named follow-on, not smuggled in via the fixtures.
- ModelFuzz-style model-guided cell functions for specific protocols — the named follow-on above.
- Any `Archive`/`Selector`/`Tactic` change, or edits to `dissonance/explorer`.
- Multi-line/structured log parsing beyond whitespace tokenization — v1 is line-oriented.
