# IMPLEMENTATION — tasks/120, differential-lineage spike (`hm-bbx.2`)

Mechanics, deviations, and integrator notes only. **Findings and the GO/NO-GO
recommendation live in PR #121's description** (the tasks/120 rule);
measurements live in `REPORT.md`; component layout and the query-family →
test map live in `README.md`.

## What this is

A standalone Cargo project (detached from the workspace, tracked via a root
`.gitignore` exception like `spikes/nested-x86`) proving the eight hardest
Differential queries of the Dissonance observation/materialization plane over
persisted fixture records, with the cost story measured. Dependencies
(ask-by-comment declaration): `differential-dataflow` 0.24.0, `timely`
0.30.0 (same resolved versions as the proven `ivm-fork-oracle` spike, pinned
by the committed `Cargo.lock`), `serde`, `serde_json`. No `rand` (splitmix64
inline, caller-seeded), no floats, no `unsafe` (memory reporting uses `ps`
plus a documented `/usr/bin/time -l` wrapper).

Doctrine compliance (per the spec's blocking rules):

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

## Structural validation (r1 review)

`Fixture::validate` (data.rs) enforces the contracts every consumer relies
on; `dataflow::run` and `Referee::new` refuse fixtures that fail it, and
`tests/validate.rs` covers each rejection:

- lineage is a forest per config (no self-parent, one parent per child, no
  cycles — a cycle would keep the ancestry iteration from converging);
- no record commits at `Revision::MAX` (the driver advances to `rev + 1`);
  the driver walks the sorted **occupied** revisions, not every integer, so
  sparse revision numbers cost nothing;
- persisted positions are the contiguous suffix range from the branch-point
  count (the restored prefix is inherited, never re-persisted);
- no cut of any kind precedes its rollout's branch point or exceeds its
  persisted extent — the **physical cut contract** lineage composition is
  sound under (a machine exists only from its branch moment onward; the
  parity harness caught the random generator violating this in development).

## Metering (r1 review — corrected)

Per-stage update counts (`Captured::deltas`) include every reduce **input**
stage (`shared.units`, `shared.start_in`, `shared.obs_in`,
`naive.point_anc`) and the ancestry stages (`lineage.anc` plus
`lineage.step`, the inside-the-iteration join churn, attributed to its outer
revision via the `Product` timestamp's outer component). The benchmark's
per-branch marginals sum the formulation's stages plus the lineage stages.
The original output-only metering understated shared-formulation cost by
hiding the depth-linear inheritance term — see `REPORT.md`'s correction
section for the restated numbers.

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
- **`(revision, Moment)` product timestamp** — rejected by doctrine and by
  measurement (REPORT.md).
- **Hashed cells** — rejected; `CellKey` is an exact sorted vector, so tests
  assert real values and no hash order can leak. The spike's `cell_fn` is
  deliberately *not* a `CellFn` ratification.
- **Counting global allocator for memory metrics** — rejected (needs
  `unsafe impl GlobalAlloc`; rule 7 grants none): `ps`-sampled RSS plus the
  documented `/usr/bin/time -l` wrapper covers the memory line.
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

## Gates (all green on this manifest, macOS; mirrored in CI)

```
cargo build            # + --release
cargo test --locked    # 27 tests: exact (11) + parity (7) + validate (9)
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo fmt -- --check
cargo deny check --config <root>/deny.toml licenses
cargo run --release --locked --example bench   # REPORT.md numbers
cargo run --locked --example gen_fixtures      # byte-identical (asserted in tests)
```

CI: the `spikes/differential-lineage gates (out-of-workspace manifest)` step
in `.github/workflows/quality.yml`'s `gates` job (added on this branch
against current main — flagged in the PR for reconciliation with the
in-flight quality.yml migration, PR #118). Root `clippy.toml` determinism
lints apply via directory discovery; the one `Instant::now` use is the
benchmark's wall-clock reporting, annotated `// not order-observable:` per
convention. No Miri obligation (no `unsafe`).
