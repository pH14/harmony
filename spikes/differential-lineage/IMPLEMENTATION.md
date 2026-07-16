# IMPLEMENTATION — tasks/120, differential-lineage spike (`hm-bbx.2`)

## What was built

A standalone Cargo project (detached from the workspace, tracked via a root
`.gitignore` exception like `spikes/nested-x86`) proving the eight hardest
Differential queries of the Dissonance observation/materialization plane over
persisted fixture records, with the cost story measured. Dependencies (ask-by-
comment declaration): `differential-dataflow` 0.24.0, `timely` 0.30.0 (same
resolved versions as the proven `ivm-fork-oracle` spike), `serde`,
`serde_json`. No `rand` (splitmix64 inline, caller-seeded), no floats, no
`unsafe` (memory reporting uses `ps`, peak footprint via `/usr/bin/time -l`).

Doctrine compliance (explicit, per the spec's blocking rules):

- **Branch is a key**: one dataflow; rollout identity appears only in keys
  and data columns; forks add lineage *rows*.
- **Revision is time; Moment/ordinal is data**: the only DD timestamp is the
  `u64` campaign revision a committed input update carries; every V-time
  coordinate `(Moment, pos)` is a data column. A revision numbers a committed
  input update, not a rollout (fixtures commit a rollout's evidence, its
  seals, and its entry commits at different revisions — the two-pass tests
  depend on it).
- **No custom lattice**: outer timestamp `u64`; the only nested timestamp is
  the standard `Product<u64, u64>` inside `iterate` (lineage closure).

## PR description draft (findings + GO/NO-GO — lift into the PR)

**Deliverable status: all eight query families proven over committed fixtures
with exact expected outputs; cost measured; recommendation GO.**

1. **Lineage-complete prefixes at candidate seals** — composed from ancestor
   segments (half-open `[parent cut, child cut)` on the persisted vector
   position) plus the child suffix, via an iterated lineage closure joined
   against one shared evidence arrangement. Equal to the genesis-replay
   referee ("replay is the semantic oracle") on every candidate seal of every
   generated tree, at every revision (`tests/parity.rs`), and to hand-written
   exact rows (`tests/exact.rs`).
2. **Provisional transitions at configured unsealed cuts** — first-pass
   cell transitions baselined at the inherited branch-point cell; they
   nominate replay and are *structurally* unable to reach occupancy
   (occupancy reduces only committed entries joined to derived seal cells).
   The `two_pass` fixture drives the full two-revision barrier: provisional
   at r2, seal with state drift at r3 (sealed cell ≠ observed cell), commits
   at r4 (quality tie broken by entry id), quality-domination flip at r5 —
   each stage read behind the probe and asserted exactly.
3. **Sibling-safe rollout identity** — same-cut siblings reuse vector
   positions; rollout identity in every key keeps coordinates disjoint
   (exact test: same `(pos, Moment)`, different owners/values, identical
   inherited prefixes, zero leakage).
4. **Same-`Moment` cuts** — the `(Moment, included SDK-event count)` cut is
   half-open on position; fixtures straddle cuts with same-`Moment` clusters
   at both a fork cut and a seal cut; boundary events are neither duplicated
   nor dropped (exact prefix rows + the reg-12 inheritance assertion).
5. **Canonical order reconstruction** — the runtime holds a multiset; reads
   consolidate then sort on explicit `(Moment, pos)`; unit multiplicity is
   asserted on every set-like view; equal payloads at different coordinates
   stay distinct.
6. **set/max/min/accumulate + history** (`count`/`ever`/`latest`) — declared
   per register in the schema stand-in; all combines commutative/associative,
   which is what lets segment aggregates compose through lineage; exact
   values asserted at every point of the hand tree, including the
   R13-absent-at-seal case and accumulate's dedup.
7. **Property-level assertion aggregation** — evaluations aggregate by
   property across sites (one row for two sites); site coverage is a separate
   provenance view; a never-satisfied `must_hit` property is a finalized
   absence finding derived from declarations minus satisfied properties.
8. **Record-class separation** — immutable evidence (insert-only ledger),
   bounded working membership (±1 updates; the only retractable input),
   committed Entry assignments, and finalized property facts are separate
   inputs/views; expiring a working coordinate changes only the working view
   (bit-identical occupancy/cells/property/absence across the retraction
   revision, asserted). Cross-source sequence queries answer only
   rollout-global sources and *reject* the source-local scrape (which stays
   reportable as terminal evidence).

**Cost story** (`REPORT.md`, reproducible via
`cargo run --release --example bench`): per-branch incremental cost is flat
in prefix depth under the segment-shared formulation — 464→479 updates per
branch on a 40-deep chain where the naive prefix-join formulation grows
637→62,510 (81× total, 10× wall); a late mid-segment seal costs 338 updates
(∝ own segment) vs 15,599 naive (∝ full prefix); maintaining all views
incrementally beats per-revision direct recompute 385× (72 ms vs 27.7 s) and
even a single final recompute by 33×. Peak process footprint 192 MB for the
whole benchmark. Update counts deterministic across reruns (asserted).

**Findings a production design must carry:**

- **Physical cut contract**: lineage composition by `pos < bound` is sound
  because no cut precedes its rollout's branch point (cuts nondecreasing
  along lineage paths) — physically guaranteed by the VM, enforced by the
  fixture builder, and *caught by the parity harness* when the random
  generator violated it. Production ingestion should reject violations as
  malformed evidence.
- **Boundary insertion re-keys the split segment** (no native interval join
  in DD): cost bounded by own-segment length, measured small (the 338-update
  late seal); if production segments dwarf spike segments, pre-declare
  candidate boundaries in `CampaignConfig` or accept the segment-linear
  re-key.
- **RunTrace suffix-only cross-fork contract gap** (named landmine from the
  prior spike): sidestepped by construction here — the persisted ledger
  stores only each rollout's own suffix under its own identity, and the
  *lineage relation is the composition authority*; nothing ever needs a
  cross-fork trace read. The restored prefix is inherited, never re-inserted
  (the ingestion rule the strategy pins), so the suffix-only property of
  per-rollout capture is a feature, not a gap, in this plane.
- **Nested V-time scope: not justified.** Flat per-branch cost is a keying
  discipline (segment aggregates at boundary granularity), not a timestamp
  property. Recommend against adding one; re-open only if a measured
  workload contradicts this.

**GO/NO-GO: GO** — production `differential-dataflow` (0.24/timely 0.30),
one worker, revision-timestamped, branch-as-key. Ratification: `hm-bbx.5`.

## Compatibility with `hm-bbx.1` (sdk-events)

The persisted tuple contract carries exactly the identity components the
epic pins: campaign/config, rollout, source, site/property (assertions),
`Moment`, explicit vector-position ordinal, plus declared source ordering
scope and per-register base update operations. `SdkEventRec`/`RegisterDecl`/
`SourceDecl`/`PropertyDecl` are fixture stand-ins for normalized
`SdkSchema`/`SdkEvent`; swapping in the sdk-events types is a field-level
mapping, not a shape change. Undeclared registers stay evidence without
becoming reducible state (both formulations and the referee drop them from
reduction, mirroring the v1 never-fired rule).

## Deviations considered and rejected

- **Branch-per-dataflow (prior spike's shape)** — rejected: the strategy
  pins the observation plane to one dataflow with a campaign-revision
  timestamp; the prior spike's shape is the *oracle* species.
- **`(revision, Moment)` product timestamp** — rejected by doctrine and now
  by measurement (see REPORT).
- **Hashed cells** — rejected; `CellKey` is an exact sorted vector, so tests
  assert real values and no hash order can leak. The spike's `cell_fn` is
  deliberately *not* a `CellFn` ratification.
- **Counting global allocator for memory metrics** — rejected (needs
  `unsafe impl GlobalAlloc`; rule 7 grants none): `ps`-sampled RSS plus a
  documented `/usr/bin/time -l` wrapper covers the bead's memory line.
- **`rand` dev-dependency** — rejected; inline splitmix64 is smaller and
  pinned.

## Known limitations

- Sequence-pair queries enumerate ordered pairs per owning-rollout segment
  (quadratic in note count per rollout) — fixture-scale by design; a
  production sequence predicate would be windowed or pattern-compiled.
- Cross-source sequencing is per owning-rollout segment, not across the
  composed lineage prefix (adequate to prove the ordering-scope rejection
  contract; lineage-composed sequences would reuse the family-1 machinery).
- A query naming an *undeclared* source is dropped by the DD join and
  rejected by the referee; fixtures always declare sources. Production
  should make undeclared-source queries a typed validation error.
- Single worker, single process throughout (matches the epic's initial
  design note).
- `Agg::combine` panics on a dimension-kind mismatch, which is unreachable
  from any input this spike accepts (the dimension fixes the constructor);
  a production ingestion layer would type this away per hm-bbx.1.

## Gates (all green on this manifest, macOS)

```
cargo build            # + --release
cargo test             # 18 tests: exact (11) + parity (7), ~2.5 s
cargo clippy --all-features --all-targets -- -D warnings
cargo fmt -- --check
cargo run --release --example bench      # REPORT.md numbers
cargo run --example gen_fixtures         # byte-identical (asserted in tests)
```

Root `clippy.toml` determinism lints apply (directory discovery); the one
`Instant::now` use is the benchmark's wall-clock reporting, annotated
`// not order-observable:` per convention. No Miri obligation (no `unsafe`).
