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

For first-event studies, `CampaignExecutionOptions::stop_after_milestone` accepts
a canonical name returned by `Evaluation::named_milestone`. The workload owns
the vocabulary, observation-policy version and first admitted execution. The
coordinator records that criterion in the header, stops new reservations after
the first observed admission, and drains already-reserved jobs normally. It
does not change rollout termination, selection, retention or pre-event decisions.
`first_milestone.frames_emulated` charges bootstrap and complete jobs through
that admission; it is not an exact within-job event timestamp. The final frame
total includes the remaining drain. Replay reconstructs the same event cost,
checks the observation-policy version and rejects jobs beyond the allowed drain.
Absent options preserve the previous stream/report fields and stopping behavior.
Callers must compare the event cost with their frame cap: an event in post-budget
drain is observed evidence, not a budgeted hit.

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

`energy_splice_continuation_v2:<scale>` applies the same separate accounting
with ordinary energy-splice mutation. Its continuation outcomes neither reward
nor penalize the ordinary splice strategy. Version 1 had credited those outcomes
to splice energy, so its existing comparisons describe that combined mechanism;
they do not isolate the effect of triggered replay. The v2 identifier enables a
paired test of the separation while preserving recorded v1 behavior.

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

`room_cell_uniform_128_energy_progress_no_cost_v1:<thresholds>` is an ablation of
`energy_progress_cheapest_v1`. It removes the between-cell historical cost rank
and makes the newest sampleable within-cell window uniform. It preserves the
semantic class walk, novelty ranks, barren energy, entry exhaustion, uniform
quarter, retention and suffix generation. Here cost is elapsed execution time
since entering the coarsest group, not global route length or snapshot replay
work. The existing `(time_in_group, id)` order still couples random draws when
weights match; the new within-cell distribution is uniform even across cost
ties. Removing cost also removes its age tiebreak and changes the effect of
novelty where the old combined rank reached its cap. This is one combined cost
rank ablation, not an attribution between those effects.

The optional `selector-cost-audit` feature emits `selector_cost_diagnostics` in
progress sidecars for the uncoun-ted semantic cost/no-cost comparison. It compares
the two normalized weight vectors at encountered between-cell and within-cell
selection stages using exact integer arithmetic, reporting total variation
rounded down to millionths, nonzero-change counts, cap terms and equal-cost rank
boundaries. Counters include dispatched work not yet admitted. They never alter
weights, random draws, reports or campaign streams. Persistent counters occupy
112 bytes; transient vectors are bounded by the existing group/window lengths
and are additional to the historical logical archive budget. Process RSS includes
their physical cost. Conditional differences describe one encountered archive;
they are not a bound on an adaptive campaign's improvement. Feature-off sidecars
omit this optional field.

## Retention diagnostics

`Reporting::observe_retention` can inspect a same-slot competition before the
incumbent is removed. The read-only event includes cached snapshots, prior
selection exposure, and lazy reconstruction of both inputs. Observers must
bound their storage and account for reconstruction separately; observer input
materialization never changes deterministic reconstruction counters. The
same event offers a lazy complete local-slot view with stable ids, optional
cached snapshots, inputs and the local rule's proposed keep/remove flags. No
slot vectors or inputs are allocated unless requested. The proposal precedes
global population and memory eviction, so it does not certify the final global
survivor set. Missing cached snapshots remain explicit members of the view.
The constant-size `retention_diagnostics` sidecar census counts removal before
window exposure, recorded parent selections and productive-admission credit.
Selections include pre-execution duplicate skips, which execute no new job.
Pending job credits and continuation within the birth job are not represented,
so zero credit does not prove that no outgoing action was executed. It is not
replay state.
The evaluator flushes/disables campaign sampling before verification replay.

The opt-in `resource_extremes_2_v1` slot policy keeps at most two resource
extremes supplied by `ArchiveKey::retention_resources`, breaking ties by
existing group cost and stable entry id. It preserves the best point under
each axis ordering, not every Pareto point. Retained alternatives share the
ordinary archive byte budget and selector. Unsupported keys use their ordinary
rule. The stream header records the policy; omission replays the legacy rule.
This is an experimental mechanism, not a default or a behavioral dominance claim.

`resource_coverage_2_v1` is a separate two-representative experiment. It chooses
the subset covering the most nonnegative integer threshold pairs under the
two resource axes, including zero thresholds. Unlike coordinate extremes,
it can retain intermediate tradeoffs. At each competition it considers only
the current representatives and candidate; it is not globally optimal over
discarded history. Equal coverage prefers fewer representatives, then sorted
within-group cost/stable-id pairs. Exact integer arithmetic covers the entire
u64 axis range. Missing axes use ordinary retention. The same memory budget,
parent selector, continuation behavior and recorded replay rules apply; the
resource coverage proxy does not prove behavioral dominance or task success.

`representative_job_sample_2_v1` instead retains the ordinary best representative
and the best candidate from the job cohort with the lowest fixed mixed rank.
The rank uses the already recorded creation execution, consumes no campaign RNG,
and adds no persistent per-entry state. Equal cohort ranks prefer ordinary
quality, group cost and stable id. Coincident winners need one entry; otherwise
the slot holds two under the same archive budget. This policy needs no resource
axes. Between external evictions it preserves these two extrema of the seen
stream, assuming a stable total quality order. It is not uniform sampling of
physical states: candidates within a job share a rank, the hash is fixed, and
search arrivals depend on earlier retention. Imports and evictions further
limit any sampling interpretation. The parent selector remains unchanged.

`quality_representatives_2_v1` keeps the top two ordinary quality/cost/arrival
representatives. `context_representatives_2_v1` instead keeps the top two
quality maxima from distinct `ArchiveKey::retention_context()` values. Context
values have only equality semantics; their numeric order never supplies a
preference. Coincident contexts need one representative. Missing context on
any competitor uses the ordinary rule for that competition. Both mechanisms
keep at most two entries under the same byte budget and leave selection groups
unchanged. The first is a capacity control for the second. For fixed streams,
the latter preserves the two highest-quality context maxima between external
evictions/imports; neither rule guarantees useful future behavior. Replay
records the explicit policy identifier and rejects unknown identifiers.

Retention lifecycle diagnostics reuse existing selector exposure vectors and add
only fixed counters, reported by `retention_diagnostic_memory_bytes`. Existing
vectors remain covered by archive metadata charging. Measured process RSS also
includes workload-owned audit storage. The final census reads only cached
active endpoints; missing payloads are counted and never reconstructed.
The final `retention_context_census` separately counts active retained keys,
including keys whose snapshot payload is missing. It excludes historical
snapshot anchors and reports same/distinct-context pairs per slot. Its temporary
grouping storage is proportional to active keys, used only for final telemetry;
it does not affect selection or imply useful future coverage.

The abstraction fixtures in `src/search/archive_abstraction_tests.rs` use exact
finite transition systems with the production admission rules. They distinguish
lost continuation events from endpoint equality, show capability aliasing and
the limits of coordinate-extreme retention, and check stable partition
refinement against exhaustive product-graph equivalence. These are finite
counterexamples, not correctness proofs for workload keys. The assumptions and
research predictions are in [`retention theory`](../../benchmarks/search/retention-theory/theory.md).
