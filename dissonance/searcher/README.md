<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# searcher

`searcher` implements deterministic search independently of a workload. The
`search::` modules own archive retention, parent selection, input mutation,
campaign coordination, worker execution, seeded draws, checkpoints, stream
recording, and replay. Workloads supply associated types through `CampaignTypes`
and implement four contracts. `Game` composes those contracts for a full campaign.

The archive groups entries at several ordered depths. A workload provides the
key and any same-location state preference; the generic archive uses only the
resulting ordering and retains bounded representatives. Campaigns reserve jobs
in a deterministic admission window, allow physical workers to execute them,
and process results in recorded admission order. The stream records the
configuration, policies, origins, jobs, admissions, skips, and progress needed
for replay. Reserved jobs pin the snapshot they actually restore, including a
parent's keyframe. If retention removes that snapshot from the active population,
its memory stays charged until the last reservation is admitted. Live execution
and serial replay release these pins at the same recorded boundary, independent
of worker completion timing. Budgeted streams before schedule version 3 are
rejected because they used different snapshot accounting.

Physical executors default to at most one running or completed-but-unadmitted
job each. `run_campaign_checkpointed_with_options` can explicitly allow two
through `ResultBuffering::TwoPerWorker`. Credits return only at ordered
admission, so a fast worker cannot accumulate unbounded completed snapshots.
This overlaps already-reserved work; it does not change the logical window,
selection order, snapshot pins, or deterministic campaign bytes. The default
remains appropriate for large whole-VM results. Additional worker-result
memory is outside the archive's logical budget and must be measured in host RSS.
Benchmark callers record this physical execution choice in their run identity.
A wall-time stop, unlike a fixed work ceiling, can change with execution speed.

## Workload boundary

`searcher` is independently buildable. Workload packages implement its typed
campaign and target contracts:

| Contract | Workload responsibility |
| --- | --- |
| `TargetExecution` | Construct, drive, restore, and snapshot targets; capture observations and account for execution cost. |
| `InputPolicy` | Define the action vocabulary, draw suffixes, retain policy history, and checkpoint draw state. |
| `Evaluation` | Classify outcomes, derive archive keys, and accumulate progress and evidence. |
| `Reporting` | Identify and serialize recordings and assemble archive reports. |

Each contract depends on `CampaignTypes` and can be implemented independently.
A complete adapter receives the aggregate `Game` implementation automatically.
The `tests/interfaces.rs` fixture implements execution alone and exercises it
through a function bounded only by `TargetExecution`.

Workloads can expose bounded observation counters through `Reporting::diagnostics`.
The engine places them only in the live progress sidecar. They never influence
selection, admission, or deterministic reports. Witness evaluators may collect
the same observations through `Reporting::merge_witness_diagnostics`, whose
contract excludes champion selection and input publication. Workloads must document
their scope and bound their memory; these observer allocations are reflected in
RSS rather than the archive's logical memory budget.

The shared `search::rollout` loop owns suffix
limits, action evidence capture, candidate creation, retention probe placement,
and stopping; a workload provides action execution and state evaluation.

The NES package lives in `../../workloads/nes`. It owns game adapters, emulator
integration, and campaign binaries. A probe must restore candidate state before
returning, including adapter caches and pending input.

Run the core checks with:

```sh
cargo test --manifest-path dissonance/searcher/Cargo.toml
cargo clippy --manifest-path dissonance/searcher/Cargo.toml --all-targets -- -D warnings
```

## Search evaluation policies

The legacy selector identifiers retain their exact behavior. Search experiments
use independent versioned identifiers:

- `room_cell_uniform_128_energy_frontier_cheapest_count_v1:<thresholds>` divides
  each within-cell cost weight by one plus that entry's admitted selections.
  Cheap members get early attempts, while repeatedly sampled members yield some
  probability to alternatives. No workload field is added.
- `energy_splice_continuation_v1:<scale>` retries transitions learned during the
  current campaign when a strictly preferred state replaces a same-slot holder.
  At most one in four reservations can do this; empty queues use ordinary energy
  splice draws. This is continuation replay: applying a previously discovered
  action tail from a new state and evaluating the resulting state normally.
  It is distinct from verification replay, which checks a recorded execution.
- `alphabet_continuation_v1` uses the same bounded, quarter-share learned exits
  with alphabet-only ordinary draws. A retry increments only continuation
  accounting and the cache-use bit; it does not consume entry/key selection
  counts, mark exploration barren, or reward the ordinary mutation strategy.
  Results still pass through normal retention and may trigger another improved
  same-slot continuation. This separates route repair from ordinary exploration
  without adding a workload preference tier. The older energy-splice continuation
  identifier preserves its original combined accounting and mutation behavior.

The continuation bank retains at most 8,192 observed exits, eight destinations
per source slot, 128 actions per exit, and 1,024 pending attempts. It charges a
fixed conservative capacity reserve against the logical memory budget before
bootstrap. Pending attempts do not pin historical snapshots: stale parents are
skipped. Dispatch records the complete action tail, so later donor reclamation
cannot change serial replay. Only same-slot `preference_cmp` is consulted;
preferences are never compared between unrelated locations. A workload that
reports no preference improvements gets no continuation attempts.

These are experiments, not new defaults. Promote policies based on paired game
panels, fresh SMB completion, and resource costs through
[`benchmarks/search`](../../benchmarks/search/README.md). The generic resource
fixture exercises actual continuation dispatch, snapshot eviction, concurrent
reservations, exact report/checkpoint replay, and planted recording corruption
without an emulator or ROM.

`room_cell_uniform_128_energy_frontier_cheapest_key_count_v1:<thresholds>` is a
separate count-history experiment. It uses the larger of an entry's selection
count and the remembered count of its depth-0 retention key. A cache of 16,384
recently selected keys survives entry replacement and metadata compaction within
the campaign. Least-recently-selected keys are evicted when it fills; an entry's
own count remains a floor. Counts saturate and a fixed conservative reserve for
both ordered indexes is charged before bootstrap. Recorded skips also count as
selections. Reports include capacity,
occupancy, cache hits, evictions and that reserve. The cache starts empty for a
new campaign, including an archive-origin run, and never pins old entries.

This tests whether archive churn repeatedly gives an already-sampled state a
fresh sampling count. It also carries history across same-slot resource
improvements, which may reduce their ordinary draw share; the companion game
panels must check that tradeoff. No default change is implied by the mechanism.

Progress sidecars carry objective workload evidence, actual admitted execution
frames, final totals, logical memory categories, and monotonic host time. With
`HARMONY_COORDINATOR_PROFILE=1`, they also contain coordinator phase durations
and dispatched replay/suffix action budgets. Those action budgets are requested
time, not actual emulator frames. Profiling values and clocks never enter
search decisions or the deterministic campaign stream.

`room_cell_uniform_128_energy_progress_cheapest_count_v1:<thresholds>` is a
separate experiment that uses `ArchiveKey::progress_cmp` for class preference
and frontier weighting. Equivalent/incomparable coarsest classes share draws;
identity still orders maps, never the potentially partial progress relation.
Each selection draws one maximal eligible class; it does not fall through to
other classes when that class yields no cell. Semantic frontier rank saturates
at 16, matching the weighting span, rather than counting the entire tail.
Within a pooled subtree it chooses a maximal observed descendant as its progress
representative. Generic tests relabel locations and expose the numeric-label
bias in the legacy control. This policy changes parent selection; ordinary
splice donor ranking retains its historical key ordering and remains a separate
ablation concern for nonlinear workloads.

`run_campaign_checkpointed_with_frame_budget` adds an optional deterministic
admitted-frame cutoff without changing existing `CampaignConfig` callers. The
stream and report record that budget only when present. Already reserved jobs
drain normally; evaluators must score first-victory cost against the threshold,
not treat a later victory from the drained window as a budgeted success.

`room_cell_uniform_128_energy_progress_cheapest_v1:<thresholds>` isolates semantic
progress weighting from entry-count weighting. It uses the same progress walk
and cheapest-cell preference as the count variant, with the original per-entry
weights. This recovers the location-neutral frontier behavior of the historical
Metroid Pareto experiment: within an inventory class, its declared progress
relation considers map cells equal, so no map cell can dominate another. It is
not the full historical cross-location preference/Pareto implementation, and
it does not restore the prototype's improvement-replay queues. Its separate
identifier permits an ablation without changing any existing selector's behavior.
