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
deterministic — the benchmark asserts identical per-revision counts across
reruns of the shared formulation, and `tests/parity.rs` asserts identical
raw update streams for the full both-formulations build; wall times are
single-run spike-grade numbers (±10% run to run on a quiet machine).

## r6 baseline correction (review finding — the recompute comparison shrinks)

The direct-recompute baseline used to call `obs`/`cells`/`transitions`/
`occupancy` independently, re-folding each evaluation point's prefix 3–4×
per revision while the dataflow shares intermediates — overstating recompute
cost by ~4×. The baseline is now ONE coherent snapshot per revision
(`Referee::snapshot`: one fold per point, every view derived from it;
equality with the individual views asserted across the parity suite).
**The GO-supporting recompute ratios move substantially and are restated:**
per-revision recompute vs incremental is now **~52× (deep chain)** and
**~2.5× (wide tree)** — previously claimed ~390× and ~10× — and a one-shot
final-revision recompute is ~7× slower than the whole incremental run on
the deep chain but **~8× FASTER on the shallow wide tree** (8.6 ms vs
71 ms): for shallow campaigns whose views are read once at the end, a
coherent recompute is competitive-to-better, exactly echoing the prior
fork-oracle spike's "batch wins one-shot small-clean" finding. The
formulation-level evidence (update-count marginals, deterministic and
unchanged) and the per-revision-currency read pattern — the Explorer reads
its views after every revision barrier — carry the GO; the recompute column
no longer does the heavy lifting, and direct recomputation remains the
semantic oracle per the strategy.

Every table figure is recomputable from the committed raw artifact
`bench-results.json` (per-stage per-revision update counts for every
isolated run, plus marker revisions): attributable(rev) = Σ counts whose
stage starts with the run's prefix or `lineage.`.

## r5 accounting correction (review finding — numbers restated again)

r1 added the lineage meters but the benchmark's displayed totals and
per-branch figures still summed only the formulation prefixes — the r1
correction text claimed lineage work was included when the sums excluded it.
The sums are fixed (`attributable = formulation stages + lineage stages`),
the full accounting is now printed under every table row (lineage, base
ingestion, downstream common views — nothing metered is silently excluded),
and every figure below is from the corrected run. The shift: the shared
formulation's deepest-branch marginal is 935 (was displayed 858), the depth
slope ≈ 11.4 updates/ancestor (was displayed ≈ 9.5), naive's slope ≈ 1,593
(unchanged — lineage stages are two orders of magnitude below its own), so
the slope ratio is ≈ 140× (was claimed ≈ 170×) and the deepest-branch
absolute ratio 67× (was 73×). No conclusion flips; the numbers move a few
percent against the shared formulation and are restated everywhere.

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

"Attributable updates" = records flowing through every metered stage under
the formulation's prefix (reduce inputs and outputs: `naive.*` / `shared.*`)
**plus the lineage stages** (`lineage.anc` and `lineage.step`), post-operator,
per revision — summed exactly this way in the table's total and per-branch
columns (r5). Base ingestion (`base.measures`) and the downstream common
views are identical across formulations and reported separately under each
table; nothing metered is silently excluded.

## Results (honest metering)

### deep-chain — 16,000 events, 40 branches, 41 candidate seals, 160 evaluation points

| formulation | attributable updates (formulation + lineage) | wall | first branch | median branch | deepest branch | seal wave | late seal |
|---|---|---|---|---|---|---|---|
| naive (per-point prefix join) | 1,600,002 | 992 ms | 637 | 31,060 | 62,742 | 328,239 | 15,638 |
| shared (segment aggregates) | 30,690 | 142 ms | 489 | 740 | 935 | 954 | 368 |
| direct recompute (one snapshot per revision) | 1,908 rows final | 7.4 s (final revision alone: 958 ms) | — | — | — | — | — |

Of which, in both isolated runs: lineage stages 1,521 total (0 at the first
branch, 77 at the deepest — the ancestry closure and its per-iteration join
churn); base ingestion 16,000 (common, excluded from the columns above);
downstream common views (cells/transitions/occupancy/properties/…) 316.

### wide-tree — 12,000 events, 60 branches, 61 candidate seals, 241 evaluation points

| formulation | attributable updates (formulation + lineage) | wall | first branch | median branch | deepest branch | seal wave | late seal |
|---|---|---|---|---|---|---|---|
| naive | 161,286 | 110 ms | 175 | 1,922 | 1,969 | 38,197 | 512 |
| shared | 30,351 | 71 ms | 286 | 477 | 435 | 1,550 | 108 |
| direct recompute (one snapshot per revision) | 2,876 rows final | 177 ms (final alone: 8.6 ms) | — | — | — | — | — |

Of which: lineage stages 477 total (9 at the deepest branch); base ingestion
12,000; downstream common views 476.

Process peak footprint for the whole benchmark (both shapes, all stages,
fixtures and referee included): **~200 MB** max RSS (`/usr/bin/time -l`).
Wall times in this table are the committed `bench-results.json` run, taken
under residual background load (a sibling worker's mutation testing);
quieter reruns measured deep-chain shared as low as 70 ms and naive 664 ms —
update counts are byte-identical across every run, walls are context.

## Reading the numbers

1. **Per-branch incremental cost is own-segment-dominated with a small
   depth-linear inheritance term.** Deep chain: 489 → 740 → 935 updates from
   first to deepest branch (≈ 11.4/ancestor, lineage stages included); naive
   grows 637 → 62,742 (≈ 1,593/depth) — a ≈ 140× steeper slope. At depth 40
   that is 52× less total work (30,690 vs 1,600,002) and 9.5× wall. The
   inheritance term is intrinsic to composing ancestor state (one ancestry
   row and its iteration churn + one cum-lookup contribution per dimension
   per ancestor); eliminating it would require materializing inherited state
   per rollout, which is exactly the recompute shape.
2. **Late materialization is cheap — the two-pass design is economically
   sound.** One later mid-segment seal on the deepest rollout costs 368
   updates (re-partitioning the one split interval + cumulative re-emission
   + point evaluation; no lineage work — a seal adds no ancestry) vs 15,638
   naive (∝ the full prefix): **42×**. This is the marginal that prices
   materialization replay.
3. **Incremental maintenance vs recompute (r6-corrected baseline):** on the
   deep chain, keeping every view current at all 43 revisions costs 142 ms
   incrementally vs 7.4 s of coherent per-revision recompute (**~52×**), and
   a single final-revision recompute (958 ms) costs ~7× the whole
   incremental run. On the shallow wide tree the per-revision gap is only
   ~2.5× (177 ms vs 71 ms), and a one-shot final recompute WINS (8.6 ms):
   incrementality pays when views must be current at every revision (the
   Explorer's read pattern — it consumes provisional transitions and
   occupancy after every revision barrier) or when prefixes are deep; a
   shallow, read-once campaign is served fine by direct recomputation, which
   the strategy keeps as the semantic oracle anyway.
4. **Boundary insertion is visible and bounded.** The seal wave (41 new
   boundaries at once) costs the shared formulation 954 updates — the
   `shared.units` re-join re-keys only split intervals — vs 328,239 naive.
   The cost is bounded by own-segment length, never prefix length.
5. **Arrangement sharing.** In the full both-formulations graph, one
   `measures-by-rollout` arrangement feeds three consumers (naive own-segment
   join, naive ancestor join, shared interval assignment); one
   `points-by-rollout` arrangement feeds four; one `evidence-by-rollout`
   arrangement feeds the two seal-prefix joins and is built only when that
   view is on (r5 — isolated benchmark runs no longer pay for an arrangement
   nothing consumes). Cloning an `Arranged` handle shares the trace, so each
   index is built and maintained once per run regardless of consumer count;
   the formulation gap above is measured *on top of* identical shared
   indexes.
6. **A nested V-time scope remains unjustified on this data.** The corrected
   numbers do show a depth term, but it is ≈ 11.4 updates/ancestor of
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
