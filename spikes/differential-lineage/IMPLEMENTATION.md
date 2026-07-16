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
by the committed `Cargo.lock`), `serde`, `serde_json`, `thiserror` (typed
validation errors, r2). No `rand` (splitmix64 inline, caller-seeded), no
floats, no `unsafe` (memory reporting uses `ps` plus a documented
`/usr/bin/time -l` wrapper).

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

## Structural validation — the complete invariant checklist (r1–r3)

`Fixture::validate` (data.rs) returns a typed `ValidationError` (thiserror);
`dataflow::run` and `Referee::new` return `Result` and refuse fixtures that
fail it — **decoded input can never panic, hang, or overflow the public
APIs**. Per the r3 mandate, this is the systematic enumeration: every
documented fixture invariant, its rule, and its test.

**Identity uniqueness**

| invariant | error | test (`tests/validate.rs`) |
|---|---|---|
| `RegisterDecl` unique per `(config, reg)` | `DuplicateDeclaration` | `duplicate_declarations_rejected` |
| `SourceDecl` unique per `(config, source)` | `DuplicateDeclaration` | `duplicate_declarations_rejected` |
| `PropertyDecl` unique per `(config, property)` | `DuplicateDeclaration` | `duplicate_declarations_rejected` |
| `LineageRec` unique per `(config, child)` | `TwoParents` | `two_parents_rejected` |
| `SdkEventRec` unique per `(config, rollout, pos)` | via `NonContiguousPositions` (a duplicated position cannot form the strict range) | `duplicate_event_position_rejected_via_contiguity` |
| `SealRec` unique per `(config, seal)` | `DuplicateRecord` | `duplicate_seal_id_rejected` |
| `ObsCutRec` unique per `(config, rollout, count)` | `DuplicateRecord` | `duplicate_obs_cut_rejected` |
| `ScrapeLineRec` unique per `(config, rollout, local_ord)` | `DuplicateRecord` | `duplicate_scrape_ordinal_rejected` |
| `SeqQueryRec` unique per `(config, query)` | `DuplicateRecord` | `duplicate_seq_query_rejected` |
| `EntryCommitRec` unique per `(config, entry)` | `DuplicateRecord` | `duplicate_entry_commit_rejected` |
| `WorkingRec` — deltas by design, no record identity; bounded by the net rule below | — | — |

**Ordering / revision coherence**

| invariant | error | test |
|---|---|---|
| no record commits at `Revision::MAX` (driver advances to `rev + 1`; walks sorted occupied revisions, sparse-safe) | `RevisionMax` | `revision_max_rejected` |
| lineage precedes its dependents: a forked rollout's events, obs cuts, seals, and any fork off it commit at/after its lineage record (chain revisions nondecreasing ⇒ the dataflow can always compose what the referee composes) | `RecordBeforeLineage` | `point_before_lineage_rejected`, `child_event_before_lineage_rejected`, `descendant_fork_before_lineage_rejected` |
| source declarations precede the sequence queries that use them (the dataflow's join waits for the declaration; a revision-filtered reader must not judge earlier) | `DeclarationAfterUse` | `query_before_source_declaration_rejected` |
| covered evidence MAY commit after its cut — deliberately legal; the referee's replay-backed views are revision-filtered to match the dataflow | — (r2 referee filter) | `parity::late_covered_evidence_staged_parity` |
| entry commits MAY precede their seal — deliberately legal; the dataflow join and the revision-filtered referee agree (nothing surfaces until both are committed) | — | exercised by `exact::family2_two_pass_occupancy_and_domination` staging |
| register/property declarations MAY commit after events — deliberately legal; both sides filter declarations by revision coherently (mirrors the v1 never-fired rule) | — | covered by every-revision parity |
| Moments nondecreasing along each rollout's own positions and across every lineage boundary (canonical `(Moment, pos)` order) | `DecreasingMoments` | `decreasing_moments_within_a_rollout_rejected`, `decreasing_moments_across_lineage_rejected` |

**Structure / positions**

| invariant | error | test |
|---|---|---|
| positions are the contiguous suffix range from the branch point (the restored prefix is inherited, never re-persisted) | `NonContiguousPositions` | `non_contiguous_positions_rejected` |
| position arithmetic is checked (`u64::MAX` cuts fail the bound, never wrap) | `PositionOverflow` | `position_overflow_is_checked_not_wrapped` |
| every cut (fork/obs/seal) within `[branch point, persisted extent]` — the physical cut contract | `CutOutOfBounds` | `seal_beyond_evidence_rejected`, `cut_before_branch_point_rejected` |
| lineage is a forest (no self-parent, no cycles — a cycle would keep the ancestry iteration from converging) | `SelfParent`, `LineageCycle` | `self_parent_rejected`, `lineage_cycle_rejected`, `run_refuses_malformed_fixture_with_typed_error` |

**Cross-record references**

| invariant | error | test |
|---|---|---|
| sequence queries name declared sources | `UndeclaredQuerySource` | `undeclared_query_source_rejected` |
| entry commits reference a seal that exists (at some revision) | `DanglingEntryCommit` | `dangling_entry_commit_rejected` |
| working updates reference a persisted evidence coordinate | `DanglingWorkingRef` | `dangling_working_ref_rejected` |
| working membership nets 0 or 1 per coordinate after every revision (admit at most once; never expire the unadmitted) | `WorkingNetOutOfRange` | `working_net_out_of_range_rejected` |
| event source ids need NOT be declared — deliberately not validated: no view consumes an event's source except through an eligible (already-validated) query, and undeclared registers stay evidence-not-state by design | — | documented here |
| referee replay coverage: every seal/obs cut on its rollout's vector, and every fork on BOTH the child's (inherited prefix) and the PARENT's vector (Fork points slice the parent — r3) | `ReplayTooShort` | `referee_refuses_short_replay_with_typed_error`, `referee_refuses_short_parent_replay` |

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
- Single worker, single process throughout (matches the epic's initial
  design note).
- `Agg::combine` panics on a dimension-kind mismatch, which is unreachable
  from any input this spike accepts (the dimension fixes the constructor);
  a production ingestion layer would type this away per hm-bbx.1.

## Gates (all green on this manifest, macOS; mirrored in CI)

```
cargo build            # + --release
cargo test --locked    # 48 tests: exact (11) + parity (8) + validate (29)
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
