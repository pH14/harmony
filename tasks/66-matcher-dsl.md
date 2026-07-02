# Task 66 — `dissonance/matcher`: the declarative signal DSL + role router

> **DELEGABLE · pure-Mac.** Most signals should be authored as config, not Rust
> (`docs/EXPLORATION.md`, "The matcher DSL"): a generic `MatchSensor`/`MatchOracle` evaluates
> declarative match expressions over any record type implementing the spine `Matchable` trait
> and routes every match by its declared **role**. This task builds that engine — expressions,
> role router, signal catalog with never-fired detection — as one plugin crate living entirely
> behind the Scoring seam. Half of `docs/EXPLORATION.md` Phase D (task 67 is the other half).
>
> Depends on **task 64** (the spine: `Matchable`, `Sensor`, `Oracle`, `RunTrace`, `Feature`, `Bug`
> in `dissonance/explorer/src/spine.rs`) having merged — dispatch only after 64 merges: the crate
> cannot compile without `spine.rs`; until then this spec is contract-only. Never redefine spine
> items; import them. Independent of 65/67; the feeding channels are later tasks (67/73/74).
>
> **Crate naming, ruled:** `docs/EXPLORATION.md`'s crate sketch says "the `match` plugin";
> `match` is a Rust keyword and an invalid package name — the crate is **`dissonance/matcher`**.

Read first: `tasks/00-CONVENTIONS.md`, `docs/EXPLORATION.md` ("The Scoring seam, elaborated",
"The three signal tiers", "The matcher DSL"), `tasks/64-explorer-spine-refactor.md` (the spine
contract, esp. `Matchable`/`Sensor`/`Oracle`), `dissonance/explorer/src/spine.rs` (once landed),
`docs/DISSONANCE.md` ("Theme is agnostic-by-interface" — it predates task 94: read Theme →
**Progression**, Variation → **Modulation**).

## Environment

Pure-logic, macOS+Linux, laptop-gated; single crate `dissonance/matcher` (hard rule 1). One
sanctioned sibling dependency: **`dissonance/explorer`** — the spine traits live there, so rule 2
(interfaces in the consumer) is satisfied by construction; depend on no other sibling. Environment
vocabulary (`Moment`, `Environment`) arrives via `explorer`'s spine re-exports; if the landed
spine does not re-export them, `dissonance/environment` is granted exactly per task 71's waiver.
Whitelist deps: `serde`+`serde_json`, `sha2`, `thiserror`, `proptest`. **No `regex`** — not
granted; matching is hand-rolled prefix/glob (below). No box, no fixtures from other tasks.

## Context

All three signal tiers converge into one `Feature` stream; this crate is the authoring layer that
makes most of it declarative. It is **replay-plane and pure**: a `MatchSensor`/`MatchOracle` is
a function of a finished `RunTrace`, never consulted mid-run (open-loop Modulation untouched).
The load-bearing invariant is **Progression blindness**: adding, editing, or deleting a signal is
a config change — it never edits the explorer loop; if anything here seems to require touching
`dissonance/explorer` beyond *importing* spine items, the spec is wrong — stop and escalate to
the foreman. A hand-written Rust `Sensor` remains the escape hatch for logic the DSL can't express.

## Config format: JSON, ruled

`docs/EXPLORATION.md` shows the signal config as YAML. That was illustrative; on disk it is
**JSON via `serde_json`**: the whitelist stays untouched, `serde_yaml` is archived/unmaintained
upstream (a poor dependency to grant), and translation is mechanical — the doc's `span:`/`log:`
sugar desugars to `kind` + an attr glob. Its example, translated (this shape is normative):

```json
{ "signals": [
  { "name": "leader.won",   "role": "sometimes",
    "match": { "kind": "span", "attr": { "name": "raft.leader_election", "outcome": "won" } } },
  { "name": "wal.lsn",      "role": "state_max",
    "match": { "kind": "span", "attr": { "name": "wal.replay" }, "attr_max": "lsn" } },
  { "name": "pg.ready",     "role": "cell",
    "match": { "kind": "log",  "attr": { "msg": "database system is ready*" } } },
  { "name": "commit.clean", "role": "never",
    "match": { "kind": "span", "attr": { "name": "txn.commit", "error": "true" }, "during": "no_faults" } }
] }
```

Expression semantics: `kind` compares exactly against `Matchable::kind()`. String attr predicates
are **globs**: `*` matches any (possibly empty) substring, no `*` means exact equality, a
trailing `*` is the prefix form — implemented as the linear-time two-pointer algorithm, no
backtracking blowup, no `regex`. `attr_max` names an integer attr to extract (the `state_max`
register input); a non-integer value is a counted decode miss, never a panic. `during` names a
context predicate; v1 ships exactly one, `no_faults`: true iff no fault `Moment` ≤ the record's
`Moment` exists in the run's fault index (`ContextSource` below). Malformed config is a typed
`thiserror` error — never a panic on untrusted input.

## Public API

As in task 64: signatures fix names/roles/semantics; parameter lists may vary, semantics preserved.

```rust
pub struct SignalSet { /* parsed, validated config; Vec<SignalDecl> in declaration order */ }
pub struct SignalDecl { pub name: SignalId, pub role: Role, pub expr: MatchExpr }
/// Stays extensible (`#[non_exhaustive]` or equivalent). Task 73's SDK catalog kinds map:
/// reachable folds to Sometimes, unreachable to Never; buggify points enter as their own kind.
pub enum Role { Sometimes, Never, Cell, StateMax }

/// The channel seam (rule 2 — defined here, in the consumer): a channel plugin (task 67/73/74)
/// pulls its record type out of a RunTrace — OWNED (or Cow), not borrowed, so a plugin can serve
/// reassembled records absent from the trace verbatim (task 74's OTLP spans). Ships test stubs only.
pub trait ChannelSource { type Rec: Matchable;
    fn records(&self, t: &RunTrace) -> Vec<Self::Rec>; }

/// Fault-Moment index for `during:`. The Environment is an opaque blob, so the production impl
/// (schema-aware, via the `environment` codec) is campaign assembly (task 69); ship a test stub.
pub trait ContextSource { fn fault_moments(&self, t: &RunTrace) -> Vec<Moment>; }

// impl spine::Sensor — the routed Feature stream for the sometimes/cell/state_max roles:
pub struct MatchSensor<S: ChannelSource, C: ContextSource> { /* … */ }
// impl spine::Oracle — evaluates the `never` rules; a match is Some(Bug):
pub struct MatchOracle<S: ChannelSource, C: ContextSource> { /* … */ }

pub struct Catalog { /* the declared signal set, any tier */ }
impl Catalog {
    pub fn from_signals(s: &SignalSet) -> Catalog;             // scrape tier: config-declared
    pub fn declare(&mut self, name: SignalId, role: Role);     // link tier: SDK-declared (task 73)
    pub fn report(&self, fired: &BTreeSet<SignalId>) -> CatalogReport;  // fired ⊎ never_fired
}
```

## The role router — exactly one declared role per signal; a match routes to that role and no other

| role | routes to |
|---|---|
| `sometimes` | a `Feature` (channel = the signal, id = fired) at the match `Moment`, + a catalog fired-mark. A hit enters the feature stream, but `Archive` admission still requires a novel `(cell, Moment)` (task 64, semantics 2) — campaigns that want per-hit checkpoint candidacy must include the sometimes channel in `CellFn`'s cell-role config. Progression stays blind. |
| `never` | a `MatchOracle` verdict: `Some(Bug)` with `fingerprint = sha2(signal name ‖ record kind ‖ matched attr bytes)` — deterministic, stable across re-derivation (the triage dedup ruling: stable coordinates, never learned cells); the scheme is **provisional** — task 75 pins the authoritative stable-coordinate `Bug` fingerprint schema and supersedes this minting site (75 lists it). Also marks the catalog fired. |
| `cell` | a `Feature` on a cell-designated channel — CellFn input (task 67 composes these). `FeatureId` = truncated `sha2` of the matched value's canonical bytes: stable across runs with **no codebook** (open-vocab codebooks are plugin-internal and this crate keeps none; a hash collision merely merges cells — safe). |
| `state_max` | the IJON register, no recompile: fold max-so-far of the `attr_max` value; emit a `Feature` whose id encodes the log2 bucket at each `Moment` the bucket increases. |

## Semantics that must hold

1. **Router totality.** Every matched event routes to exactly its declared role's consumer;
   unmatched records route nowhere; no role's output ever leaks into another's.
2. **The declared set IS the catalog.** `never_fired = declared − fired`, uniform whether a
   signal was config-declared (scrape) or `declare()`d (link, task 73) — tier-blind detection.
3. **Purity + determinism.** `MatchSensor`/`MatchOracle` are pure per `RunTrace`; output order is
   a deterministic function of record order (`BTreeMap`/`BTreeSet` or declaration order wherever
   order is observable — never `HashMap` iteration); no floats; seedless.
4. **Progression blindness.** This crate imports spine items and nothing from engine internals;
   a config change adds signals with zero explorer edits.

## Prior art (design anchors, not a bibliography)

- **IJON** (Aschermann et al., S&P 2020) [eng] — state annotations steer a fuzzer through state,
  not just coverage; the `state_max` role is IJON's `IJON_MAX` register with the annotation
  moved from source into config — no recompile.
- **SGFuzz** (Ba et al., USENIX Security 2022) [secret] — harvest state the software already
  *reifies* (pod phase, recovery state); the `cell` role exists so config authors harvest, not invent.

## Acceptance gates

1. **Standard suite** green on `dissonance/matcher` (build / nextest / clippy `-D warnings` /
   fmt / deny), all-features, macOS + Linux.
2. **Router totality proptest (≥256):** over arbitrary `SignalSet`s + record streams, every match
   appears in exactly its declared role's output — no cross-role leakage — and the routed set
   equals the set of matching (signal, record) pairs.
3. **Catalog proptest (≥256):** `fired ⊎ never_fired = declared` always, identically whether a
   signal was config-declared or entered via `declare()` (the link-tier path).
4. **Purity/determinism proptest (≥256):** the same `RunTrace` evaluated twice yields
   byte-identical serialized Feature streams and identical Oracle verdicts; permuting an
   unrelated signal's declaration never changes another signal's output.
5. **Glob proptest (≥256):** the hand-rolled glob agrees with a naive reference on random
   pattern/input pairs; no panic on any pattern, including pathological `*` runs.
6. **Config round-trip:** parse → serialize → parse is identity on the normative example above;
   each malformed-config class (unknown role, duplicate name, bad type) yields its typed error.

## Non-goals

- Any concrete channel adapter — log records are task 67, SDK/link events task 73, OTel spans
  task 74. This crate ships `ChannelSource`/`ContextSource` test stubs only.
- The production fault-index wiring — schema-aware env decoding is campaign assembly (task 69).
- `Archive`/`Selector`/`Tactic` changes, or any edit to `dissonance/explorer`.
- A regex engine, or `during:` predicates beyond `no_faults` (keep the predicate enum extensible).
- Open-vocabulary clustering/codebooks — plugin-internal by ruling (task 67 owns the first one).
