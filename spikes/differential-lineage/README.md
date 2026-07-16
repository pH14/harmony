# differential-lineage — spike (tasks/120, `hm-bbx.2`)

**Question:** can Differential Dataflow serve as Dissonance's observation and
materialization plane — lineage-complete prefixes at candidate seals,
provisional transitions at unsealed cuts, the retention record classes — under
the ruled doctrine (**branch is a key, revision is the only timestamp,
Moment/ordinal is data, no custom lattice**), at an acceptable incremental
cost?

**Answer: yes — GO.** All eight query families proven over committed fixtures
with exact expected outputs; both a naive and a segment-shared formulation
agree with a plain-Rust genesis-replay referee at every revision; per-branch
incremental cost is **flat in prefix depth** under the shared formulation
(81× fewer updates than naive on a 40-deep chain, 385× faster than
per-revision recompute); a late materialization costs ∝ its own segment, not
its prefix. See `REPORT.md` for numbers and `IMPLEMENTATION.md` for the full
findings. Ratification is `hm-bbx.5`.

Standalone: no dependency on `consonance/` or `dissonance/`. Tracked by
design (root `.gitignore` exception): fixtures, generator, and report are the
deliverable. Context: `docs/DISSONANCE-STRATEGY.md`, epic bead `hm-bbx`, and
the prior `spikes/ivm-fork-oracle` (branch-per-dataflow oracle species; this
spike is the complementary one-dataflow observation plane).

## Layout

- `src/data.rs` — persisted record model (the evidence-identity contract:
  campaign/config, rollout, source, `Moment`, explicit vector-position
  ordinal; revision is only the commit schedule) and derived-value types.
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
cargo test                            # exact + parity suites (~3 s)
cargo run --release --example bench   # the cost measurement (REPORT.md)
cargo run --example gen_fixtures      # regenerate committed fixtures
```

Gates: `cargo build`, `cargo test`, `cargo clippy --all-features
--all-targets -- -D warnings`, `cargo fmt -- --check` — all on this manifest.
