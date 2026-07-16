# Benchmark report — differential-lineage spike (tasks/120, `hm-bbx.2`)

**Reproduce with one command** (from `spikes/differential-lineage/`):

```sh
cargo run --release --example bench
```

(`/usr/bin/time -l cargo run --release --example bench` adds the process peak
footprint.) Release profile only — DD under an unoptimized profile is 10–50×
slower (prior-spike finding, reconfirmed).

**Host:** Apple M1 Max, macOS 26.4.1, rustc 1.94.1, `differential-dataflow`
0.24.0 / `timely` 0.30.0, single process, one Timely worker. Measured
2026-07-16. Update counts are deterministic (asserted across reruns in the
benchmark itself); wall times are single-run spike-grade numbers.

## What is measured

Two branch-tree shapes are grown **one rollout per revision** (the campaign
rhythm: each committed evidence batch is one revision), then a seal wave lands
(one candidate seal per rollout, its own revision), then **one late seal**
lands mid-segment on the deepest rollout — the marginal cost of a single
later materialization replay, i.e. the two-pass economics.

Two formulations of "reduced observations at every evaluation point" run in
isolation so their update counts are attributable:

- **naive** — every evaluation point joins every ancestor-segment event and
  reduces. Per-point cost ∝ full lineage prefix. This is the
  recompute-shaped baseline *inside* DD.
- **shared** — per-segment partial aggregates at boundary granularity
  (boundaries = fork cuts + configured cuts + seal counts), cumulative
  per rollout, ancestor contributions composed through the lineage
  (all combines commutative and associative). Both formulations are
  parity-tested against each other and against the plain-Rust
  genesis-replay referee at every revision (`tests/parity.rs`).

- **direct recompute** — the plain-Rust referee re-deriving every view
  (observations, cells, transitions, occupancy) from the genesis replay at
  each revision: what a non-incremental backend would pay to stay current.

"Updates" = records flowing through the formulation's metered stages
(`naive.contrib`/`naive.obs` vs `shared.units`/`shared.partials`/
`shared.cum`/`shared.start`/`shared.obs`), post-operator, per revision.

## Results

### deep-chain — 16,000 events, 40 branches, 41 candidate seals, 161 evaluation points

| formulation | total updates | wall | first branch | median branch | deepest branch | seal wave | late seal | rss after |
|---|---|---|---|---|---|---|---|---|
| naive (per-point prefix join) | 1,594,606 | 707 ms | 637 | 30,948 | 62,510 | 327,459 | 15,599 | 178 MB |
| shared (segment aggregates) | 19,668 | 72 ms | 464 | 476 | 479 | 320 | 338 | 183 MB |
| direct recompute (plain Rust, per revision) | 1,908 rows final | 27.7 s (final revision alone: 2.4 s) | — | — | — | — | — | 152 MB |

### wide-tree — 12,000 events, 60 branches, 61 candidate seals, 241 evaluation points

| formulation | total updates | wall | first branch | median branch | deepest branch | seal wave | late seal | rss after |
|---|---|---|---|---|---|---|---|---|
| naive (per-point prefix join) | 159,523 | 103 ms | 175 | 1,896 | 1,941 | 37,929 | 507 | 158 MB |
| shared (segment aggregates) | 22,275 | 68 ms | 261 | 366 | 319 | 480 | 80 | 158 MB |
| direct recompute (plain Rust, per revision) | 2,876 rows final | 656 ms (final revision alone: 29 ms) | — | — | — | — | — | 124 MB |

Process peak footprint for the whole benchmark (both shapes, all stages,
fixtures and referee included): **192 MB** max RSS (`/usr/bin/time -l`).
Per-stage "rss after" samples are cumulative within the one process — read
them as an upper bound, not per-stage cost.

## Reading the numbers

1. **Per-branch incremental cost is flat under the shared formulation.** On
   the 40-deep chain the marginal cost of adding a branch is 464 → 476 → 479
   updates from the first branch to the deepest — cost ∝ the branch's own
   segment (400 events + its points), independent of prefix depth. The naive
   formulation grows 637 → 62,510 (linear in prefix ⇒ quadratic total),
   81× more total updates and 10× wall on this shape. This is the
   revision-timestamped, branch-as-key analog of the prior fork-oracle
   spike's Window-seeding result (flat per-branch cost), achieved with no
   custom lattice and no per-branch dataflow.

2. **Late materialization is cheap — the two-pass design is economically
   sound.** A single later seal mid-segment on the deepest rollout costs 338
   updates under the shared formulation (re-partitioning the one split
   interval plus cumulative re-emission: ∝ own segment) vs 15,599 naive
   (∝ the full 15.8k-event prefix): 46×. Candidate seals arriving at later
   revisions are exactly the production pattern (observe at r1, materialize
   at r2), so this marginal is the number that matters for replay budgeting.

3. **Incremental maintenance vs recompute:** keeping every view current at
   all 43 revisions costs 72 ms incrementally; direct recompute at each
   revision costs 27.7 s (385×). Even a *single* final-revision recompute
   (2.4 s) costs 33× the entire incremental run on the deep chain. On the
   shallow wide tree the gap narrows (68 ms vs 656 ms, ~10×) — recompute
   is only competitive when prefixes stay short and views are read rarely.

4. **Arrangement sharing.** One `measures-by-rollout` arrangement feeds three
   consumers (naive own-segment join, naive ancestor join, shared interval
   assignment); one `evidence-by-rollout` arrangement feeds both seal-prefix
   joins; one `points-by-rollout` arrangement feeds four consumers. Cloning
   an `Arranged` handle shares the underlying trace, so each index is built
   and maintained once regardless of consumer count — the formulation
   difference above is measured *on top of* identical shared indexes.

5. **A nested V-time scope is not justified.** The flat per-branch cost and
   the cheap late-seal marginal are achieved with the plain total-order `u64`
   revision timestamp; prefix-evaluation cost is controlled by segment
   aggregation at boundary granularity (a keying discipline), not by
   timestamp structure. Nothing in these measurements produces a workload
   where `(revision, Moment)` product timestamps would pay for their
   progress-tracking overhead. Recommendation: do not add one.

## Caveats and findings for production

- **Boundary insertion re-keys the split segment.** When a new cut/seal adds
  a boundary, the `shared.units` join re-emits interval assignments for the
  affected events (measured: the 338-update late seal; bounded by own-segment
  length, never prefix length). DD has no native interval/ASOF join; if
  production segments grow much larger than spike segments, either
  pre-declare candidate boundaries in `CampaignConfig` or accept the
  segment-linear re-key. Measured small at spike scale.
- **Physical cut contract.** Lineage composition by `pos < bound` joins is
  sound because no cut can precede its rollout's branch point (cuts are
  nondecreasing along every lineage path) — guaranteed physically by the VM
  (a machine exists only from its branch moment onward) and enforced by the
  fixture builder. The parity harness caught the violation when the random
  generator broke it; production ingestion should validate it as malformed
  evidence.
- Single Timely worker throughout (per the epic's design note); multi-worker
  is a non-goal here and the prior spike already showed in-dataflow workers
  are the wrong axis at these work-unit sizes.
- Update counts are exactly reproducible; wall times vary run to run at the
  ±10% level on this host.

## Recommendation

**GO** on production `differential-dataflow` (0.24 / timely 0.30) as the
observation/materialization plane, under the ruled doctrine (branch as key,
revision as the only timestamp, no custom lattice), with **no nested time
scope**. The formal ratification decision remains `hm-bbx.5`.
