# differential-lineage — spike (tasks/120, `hm-bbx.2`)

**Question:** can Differential Dataflow serve as Dissonance's observation and
materialization plane — lineage-complete prefixes at candidate seals,
provisional transitions at unsealed cuts, the retention record classes — under
the ruled doctrine (**branch is a key, revision is the only timestamp,
Moment/ordinal is data, no custom lattice**), at an acceptable incremental
cost?

**Findings and the GO/NO-GO recommendation live in PR #121's description**
(single source of truth, per the tasks/120 rule); `REPORT.md` holds the raw
measurements; ratification is `hm-bbx.5`. In brief, mechanically: all eight
query families are proven over committed fixtures with exact expected
outputs; a naive and a segment-shared formulation agree with a plain-Rust
genesis-replay referee at every revision; per-branch incremental cost is
own-segment work plus ≈ 11.4 updates per ancestor (lineage stages included,
r5 accounting), against a ≈ 140× steeper per-depth slope for the naive
prefix-join and ~390× over per-revision direct recompute.

Standalone: no dependency on `consonance/` or `dissonance/`. Tracked by
design (root `.gitignore` exception): fixtures, generator, lockfile, and
report are the deliverable. Context: `docs/DISSONANCE-STRATEGY.md`, epic bead
`hm-bbx`, and the prior `spikes/ivm-fork-oracle` (branch-per-dataflow oracle
species; this spike is the complementary one-dataflow observation plane).

## Layout

- `src/data.rs` — persisted record model (the evidence-identity contract:
  campaign/config, rollout, source, `Moment`, explicit vector-position
  ordinal; revision is only the commit schedule), structural validation
  (`Fixture::validate`, returning typed `ValidationError`s through the
  public `run`/`Referee::new` Results — decoded input cannot panic or hang
  the APIs), and derived-value types.
- `src/dataflow.rs` — the DD program: one dataflow, `u64` revision time,
  13 captured views, two point-observation formulations (naive prefix-join
  vs shared segment aggregates), explicit shared arrangements, the
  revision-stepped driver with probe barriers.
- `src/referee.rs` — the plain-Rust direct-recompute referee over
  genesis-complete replay vectors (the semantic oracle).
- `src/generate.rs` — validating fixture `Builder` + seeded random tree
  generator (splitmix64; no host entropy anywhere).
- `src/fixtures.rs` + `fixtures/*.json` — the three committed hand fixtures.
- `tests/exact.rs` — exact hand-written expected outputs, one test per query
  family (the task gate).
- `tests/parity.rs` — the adjudicator: DD (both formulations) == referee on
  hand fixtures and random trees, at every revision, across reruns, under
  permuted feed order.
- `tests/validate.rs` — malformed-fixture rejection as typed errors, one test
  per invariant on the systematic checklist in `IMPLEMENTATION.md` (identity
  uniqueness, revision coherence incl. lineage-before-dependents, checked
  position arithmetic, the physical branch-point contract, Moment
  monotonicity, cross-record references).
- `examples/bench.rs` — the cost measurement (`REPORT.md`).
- `examples/gen_fixtures.rs` — regenerates the committed fixtures
  bit-identically.

## Query family → test map

| family | test |
|---|---|
| lineage-complete prefixes at candidate seals | `exact::family1_lineage_complete_seal_prefixes` |
| provisional transitions, replay-nominate never occupy | `exact::family2_provisional_transitions`, `exact::family2_two_pass_occupancy_and_domination` |
| sibling-safe rollout identity | `exact::family3_sibling_safe_identity` |
| same-`Moment` half-open cuts | `exact::family4_same_moment_half_open_cuts` |
| canonical order reconstruction | `exact::family5_canonical_order_reconstruction` |
| set/max/min/accumulate + history | `exact::family6_reductions_and_history` |
| property-level assertion aggregation | `exact::family7_property_aggregation` |
| evidence / working / committed / finalized separation | `exact::family8_retention_separation_and_ordering_scope` |
| probe / consolidate / canonical-sort discipline | `exact::probe_consolidate_canonical_sort_discipline`, `parity::*` |

## Run

```sh
cargo test --locked                            # 54 tests: exact + parity + validate (~3 s)
cargo run --release --locked --example bench   # the cost measurement (REPORT.md)
cargo run --locked --example gen_fixtures      # regenerate committed fixtures
```

Gates (mirrored in CI by the `spikes/differential-lineage gates` step of
`.github/workflows/quality.yml`): `cargo build`, `cargo test --locked`,
`cargo clippy --locked --all-features --all-targets -- -D warnings`,
`cargo fmt -- --check`, `cargo deny check --config <root>/deny.toml licenses`
— all on this manifest.
