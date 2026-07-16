# Benchmark report — differential-lineage spike (tasks/120, `hm-bbx.2`)

Raw measurements and their interpretation. The GO/NO-GO recommendation this
data feeds lives in **PR #121's description** (single source of truth, per
the tasks/120 rule); the ratification decision is `hm-bbx.5`.

**Reproduce with one command** (from `spikes/differential-lineage/`):

```sh
cargo run --release --locked --example bench
```

(`/usr/bin/time -l cargo run --release --locked --example bench` adds the
process peak footprint.) Release profile only — DD under an unoptimized
profile is 10–50× slower (prior-spike finding, reconfirmed).

**Host:** Apple M1 Max, macOS 26.4.1, rustc 1.94.1, `differential-dataflow`
0.24.0 / `timely` 0.30.0 (pinned by the committed `Cargo.lock`), single
process, one Timely worker. Measured 2026-07-16. Update counts are
deterministic (asserted across reruns in the benchmark itself); wall times
are single-run spike-grade numbers (±10% run to run).

## r1 metering correction (review finding — numbers restated)

The first published run metered only post-`reduce` **outputs** on the shared
path, which hid per-ancestor work and understated per-branch cost (it read as
flat: 464 → 479 across depth 1 → 40). The metering now also counts every
reduce **input** stage (`shared.units`, `shared.start_in`, `shared.obs_in`,
`naive.point_anc`) and the **ancestry stages** — the lineage closure's output
and its inside-the-iteration join churn (`lineage.anc`, `lineage.step`,
attributed to their outer revision) — and per-branch marginals include the
lineage stages for both formulations.

Under honest metering the shared formulation's per-branch cost is **not
flat**: it is own-segment work plus a **depth-linear inheritance term of
≈ 9.5 updates per ancestor** (ancestry row + its iteration step + one
segment-aggregate contribution per live dimension). The comparison that
matters survives: the naive formulation's marginal grows ≈ 1,590 updates per
unit depth on the same shape — a ~170× steeper slope — and at depth 40 the
shared marginal is 73× smaller in absolute terms (858 vs 62,665).

## What is measured

Two branch-tree shapes are grown **one rollout per revision** (the campaign
rhythm: each committed evidence batch is one revision), then a seal wave
lands (one candidate seal per rollout, its own revision), then **one late
seal** lands mid-segment on the deepest rollout — the marginal cost of a
single later materialization replay, i.e. the two-pass economics.

Two formulations of "reduced observations at every evaluation point" run in
isolation so their update counts are attributable:

- **naive** — every evaluation point joins every ancestor-segment event and
  reduces. Per-point cost ∝ full lineage prefix (the recompute-shaped
  baseline *inside* DD).
- **shared** — per-segment partial aggregates at boundary granularity
  (boundaries = fork cuts + configured cuts + seal counts), cumulative per
  rollout, ancestor contributions composed through the lineage (all combines
  commutative and associative). Both formulations are parity-tested against
  each other and against the plain-Rust genesis-replay referee at every
  revision (`tests/parity.rs`).
- **direct recompute** — the plain-Rust referee re-deriving every view
  (observations, cells, transitions, occupancy) from the genesis replay at
  each revision: what a non-incremental backend pays to stay current.

"Updates" = records flowing through the run's metered stages, post-operator,
per revision. Per-branch marginal columns = that branch's evidence-revision
updates across the formulation's stages **plus the lineage stages**.

## Results (honest metering)

### deep-chain — 16,000 events, 40 branches, 41 candidate seals, 160 evaluation points

| formulation | total updates | wall | first branch | median branch | deepest branch | seal wave | late seal |
|---|---|---|---|---|---|---|---|
| naive (per-point prefix join) | 1,598,481 | 727 ms | 637 | 31,023 | 62,665 | 328,239 | 15,638 |
| shared (segment aggregates) | 29,169 | 84 ms | 489 | 701 | 858 | 954 | 368 |
| direct recompute (plain Rust, per revision) | 1,908 rows final | 29.4 s (final revision alone: 2.4 s) | — | — | — | — | — |

### wide-tree — 12,000 events, 60 branches, 61 candidate seals, 241 evaluation points

| formulation | total updates | wall | first branch | median branch | deepest branch | seal wave | late seal |
|---|---|---|---|---|---|---|---|
| naive | 160,809 | 109 ms | 175 | 1,913 | 1,960 | 38,197 | 512 |
| shared | 29,874 | 70 ms | 286 | 470 | 426 | 1,550 | 108 |
| direct recompute | 2,876 rows final | 713 ms (final alone: 30 ms) | — | — | — | — | — |

Process peak footprint for the whole benchmark (both shapes, all stages,
fixtures and referee included): **~192 MB** max RSS (`/usr/bin/time -l`).

## Reading the numbers

1. **Per-branch incremental cost is own-segment-dominated with a small
   depth-linear inheritance term.** Deep chain: 489 → 701 → 858 updates from
   first to deepest branch (≈ 9.5/ancestor); naive grows 637 → 62,665
   (≈ 1,590/depth). At depth 40 that is 55× less total work (29,169 vs
   1,598,481) and 8.7× wall. The inheritance term is intrinsic to composing
   ancestor state (one ancestry row + one cum-lookup contribution per
   dimension per ancestor); eliminating it would require materializing
   inherited state per rollout, which is exactly the recompute shape.
2. **Late materialization is cheap — the two-pass design is economically
   sound.** One later mid-segment seal on the deepest rollout costs 368
   updates (re-partitioning the one split interval + cumulative re-emission
   + point evaluation) vs 15,638 naive (∝ the full prefix): **42×**. This is
   the marginal that prices materialization replay.
3. **Incremental maintenance vs recompute:** keeping every view current at
   all 43 revisions costs 84 ms; direct recompute at each revision costs
   29.4 s (**350×**); even a *single* final-revision recompute (2.4 s) costs
   29× the entire incremental run. On the shallow wide tree the gap narrows
   (70 ms vs 713 ms, ~10×) — recompute is competitive only when prefixes
   stay short and views are read rarely.
4. **Boundary insertion is visible and bounded.** The seal wave (41 new
   boundaries at once) costs the shared formulation 954 updates — the
   `shared.units` re-join re-keys only split intervals — vs 328,239 naive.
   The cost is bounded by own-segment length, never prefix length.
5. **Arrangement sharing.** One `measures-by-rollout` arrangement feeds three
   consumers (naive own-segment join, naive ancestor join, shared interval
   assignment); one `evidence-by-rollout` arrangement feeds both seal-prefix
   joins; one `points-by-rollout` arrangement feeds four consumers. Cloning
   an `Arranged` handle shares the trace, so each index is built and
   maintained once regardless of consumer count; the formulation gap above
   is measured *on top of* identical shared indexes.
6. **A nested V-time scope remains unjustified on this data.** The corrected
   numbers do show a depth term, but it is ≈ 9.5 updates/ancestor of
   *data-structural* composition work (ancestry rows and per-dimension
   contributions) — a nested timestamp scope would not remove it, because it
   is not timestamp-management overhead. What a nested scope buys is finer
   intra-rollout frontiers, and nothing here is frontier-bound.

## Caveats

- The physical cut contract (no cut precedes its rollout's branch point) is
  load-bearing for lineage composition — enforced by `Fixture::validate`,
  refused by `dataflow::run` and `Referee::new`, and tested
  (`tests/validate.rs`).
- Single Timely worker throughout (per the epic's design note); the prior
  spike already showed in-dataflow workers are the wrong axis at these
  work-unit sizes.
- Update counts are exactly reproducible; wall times are spike-grade.
- RSS is process-cumulative across stages; use the `/usr/bin/time -l`
  wrapper for the peak-footprint number.
