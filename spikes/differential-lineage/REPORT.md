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

## Verify-event correction (J3/J5 — the wide-tree exhibits are re-measured)

The tribunal judge proved the wide-tree "deepest branch" and "late seal"
exhibits measured the LAST rollout (insertion order), which equals depth
only on the chain shape: under seed 11 the last rollout sits at depth 5
while the true maximum depth is 10 (rollouts 36 and 43). The benchmark now
selects the deepest rollout by computed lineage depth (ties → lowest index;
rollout 36 here), lands the late seal there, reads the "deepest branch"
column at that rollout's evidence revision, and records the selection in
the artifact (`deepest_rollout_index`/`deepest_depth`/`deepest_rev`).
At the true deepest the naive/shared gap is LARGER than previously shown:
4,490 vs 478 (**9.4×**, was shown 4.5× at the mislabeled rollout). The
deep-chain exhibits are unaffected. Evaluation-point counts now include
fork points (J5): 199 (deep) and 300 (wide). All figures below are from
the committed artifact of this corrected run.

## Historical: r6 baseline correction (superseded figures in past tense)

The direct-recompute baseline used to call `obs`/`cells`/`transitions`/
`occupancy` independently, re-folding each evaluation point's prefix 3–4×
per revision while the dataflow shares intermediates — it overstated
recompute cost by ~4× (the pre-r6 report claimed ~390× deep / ~10× wide;
**both figures were withdrawn at r6**). The baseline has been ONE coherent
snapshot per revision since r6 (`Referee::snapshot`: one fold per point,
every view derived from it; equality with the individual views asserted
across the parity suite). r6 quoted ~52× deep / ~2.5× wide from its
artifact run; **superseded by the current artifact's figures in the tables
and reading below** (the ratio is wall-based and load-sensitive; update
counts are identical across every run). The structural conclusions stood
and stand: the update-count marginals and the per-revision-currency read
pattern carry the GO, the recompute column does not; a one-shot final
recompute wins on the shallow wide tree; direct recomputation remains the
semantic oracle per the strategy.

Every table figure is recomputable from the committed raw artifact
`bench-results.json` (per-stage per-revision update counts for every
isolated run, plus marker revisions): attributable(rev) = Σ counts whose
stage starts with the run's prefix or `lineage.`.

## Historical: r5 accounting correction (superseded figures in past tense)

r1 had added the lineage meters, but the benchmark's displayed totals and
per-branch figures still summed only the formulation prefixes — the r1
correction text claimed lineage work was included when the sums excluded
it. The sums were fixed at r5 (`attributable = formulation stages +
lineage stages`), and the full accounting has been printed under every
table row since (lineage, base ingestion, downstream common views —
nothing metered is silently excluded). The r5-shown figures (deepest-branch
marginal 935, was 858 under the broken sums; slope ≈ 11.4/ancestor, was
shown 9.5) **remain current for the deep chain** and appear in the tables
below; the pre-r5 858/9.5/~170×/73× figures **were withdrawn at r5**
(superseded: the current slope ratio is ≈ 139× and the deep-chain
deepest-branch absolute ratio 67×, both recomputable from the artifact).

## Historical: r1 metering correction (superseded figures in past tense)

The first published run metered only post-`reduce` **outputs** on the
shared path, which hid per-ancestor work: it read as flat (464 → 479 across
depth 1 → 40) — **that claim was withdrawn at r1**. Since r1 the metering
counts every reduce **input** stage (`shared.units`, `shared.start_in`,
`shared.obs_in`, `naive.point_anc`) and the **ancestry stages**
(`lineage.anc`, `lineage.step`, attributed to their outer revision); since
r5 the displayed sums include them. The r1-era restatement (≈ 9.5
updates/ancestor, ~170× slope ratio, 73× at depth 40, 858 vs 62,665) **was
itself superseded by r5**; current figures live only in the artifact-derived
tables and reading below.

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

### deep-chain — 16,000 events, 40 branches, 41 candidate seals, 199 evaluation points (incl. 39 fork points); deepest rollout: index 39, depth 39

| formulation | attributable updates (formulation + lineage) | wall | first branch | median branch | deepest branch | seal wave | late seal |
|---|---|---|---|---|---|---|---|
| naive (per-point prefix join) | 1,600,002 | 704 ms | 637 | 31,060 | 62,742 | 328,239 | 15,638 |
| shared (segment aggregates) | 30,690 | 77 ms | 489 | 740 | 935 | 954 | 368 |
| direct recompute (one snapshot per revision) | 1,908 rows final | 6.85 s (final revision alone: 625 ms) | — | — | — | — | — |

Of which, in both isolated runs: lineage stages 1,521 total (0 at the first
branch, 77 at the deepest — the ancestry closure and its per-iteration join
churn); base ingestion 16,000 (common, excluded from the columns above);
downstream common views (cells/transitions/occupancy/properties/…) 316.

### wide-tree — 12,000 events, 60 branches, 61 candidate seals, 300 evaluation points (incl. 59 fork points); deepest rollout: index 36, depth 10 (J3-corrected exhibit)

| formulation | attributable updates (formulation + lineage) | wall | first branch | median branch | deepest branch | seal wave | late seal |
|---|---|---|---|---|---|---|---|
| naive | 161,952 | 113 ms | 175 | 1,922 | 4,490 | 38,197 | 1,178 |
| shared | 30,520 | 73 ms | 286 | 477 | 478 | 1,550 | 277 |
| direct recompute (one snapshot per revision) | 2,876 rows final | 173 ms (final alone: 8.4 ms) | — | — | — | — | — |

Of which: lineage stages 477 total (19 at the deepest branch); base
ingestion 12,000; downstream common views 476. The "deepest branch" and
"late seal" columns read the true deepest rollout (index 36, depth 10);
totals differ slightly from pre-J3 runs because the late seal moved there.

Process peak footprint for the whole benchmark (both shapes, all stages,
fixtures and referee included): **~200 MB** max RSS (`/usr/bin/time -l`).
Wall times in the tables are the committed `bench-results.json` run;
observed cross-run wall variance under background load reached ±2× on some
columns (e.g. deep-chain shared 70–142 ms across runs). Update counts are
byte-identical across every run; walls are context.

## Reading the numbers

1. **Per-branch incremental cost is own-segment-dominated with a small
   depth-linear inheritance term.** Deep chain: 489 → 740 → 935 updates from
   first to deepest branch (≈ 11.4/ancestor, lineage stages included); naive
   grows 637 → 62,742 (≈ 1,592/depth) — a ≈ 139× steeper slope. At depth 40
   that is 52× less total work (30,690 vs 1,600,002) and 9.1× wall (704 ms
   vs 77 ms, committed-artifact run). On the wide tree's true deepest
   rollout (depth 10, J3): naive 4,490 vs shared 478 — **9.4×** at depth 10,
   versus 1.1× at depth 1 — depth is what the naive formulation pays for.
   The inheritance term is intrinsic to composing ancestor state (one
   ancestry row and its iteration churn + one cum-lookup contribution per
   dimension per ancestor); eliminating it would require materializing
   inherited state per rollout, which is exactly the recompute shape.
2. **Late materialization is cheap — the two-pass design is economically
   sound.** One later mid-segment seal on the deepest rollout costs 368
   updates on the deep chain (re-partitioning the one split interval +
   cumulative re-emission + point evaluation; no lineage work — a seal adds
   no ancestry) vs 15,638 naive (∝ the full prefix): **42×**; on the wide
   tree's true deepest rollout, 277 vs 1,178 (**4.3×** at depth 10). This is
   the marginal that prices materialization replay.
3. **Incremental maintenance vs recompute (coherent snapshot baseline):**
   committed-artifact run — deep chain: all views current at all 43
   revisions for 77 ms incrementally vs 6.85 s of per-revision recompute
   (**~89×**; an earlier, more loaded artifact run measured 142 ms vs 7.4 s
   ≈ 52× — the wall ratio is load-sensitive, the update counts are not),
   and a single final-revision recompute (625 ms) costs ~8× the whole
   incremental run. Shallow wide tree: per-revision gap ~2.4× (173 ms vs
   73 ms), and a one-shot final recompute WINS (8.4 ms): incrementality pays
   when views must be current at every revision (the Explorer's read
   pattern — it consumes provisional transitions and occupancy after every
   revision barrier) or when prefixes are deep; a shallow, read-once
   campaign is served fine by direct recomputation, which the strategy
   keeps as the semantic oracle anyway.
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
