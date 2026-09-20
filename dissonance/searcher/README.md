<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# searcher

`searcher` implements deterministic search independently of a workload. The
`search::` modules own archive retention, parent selection, input mutation,
campaign coordination, worker execution, seeded draws, checkpoints, stream
recording, and replay. Workloads supply associated types through `CampaignTypes`
and implement four contracts. `Workload` composes those contracts for a full campaign.

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
of worker completion timing. Campaign streams require schedule policy version 3
and the current bounded progress policy; recordings from superseded policy
namespaces are rejected before replay because their snapshot accounting differs.

Empirical step tables fold retained suffixes into an incremental hash and a
deterministic frequency map capped at 4,096 distinct steps. The compact table
is the only supported representation. Every campaign keeps one, in
`search::draw_tables`, so a workload gets the biased draw without owning any of
the bookkeeping. `DrawTables` folds each retained suffix as its record closes,
publishes an `EmpiricalStepCheckpoint` the stream records beside every draw,
and keeps the table versions a serial replay still needs. Workloads identify
their input policies and reject unknown or retired identifiers during replay.

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

`memory_budget_mib` is split before bootstrap: the workload's draw-state reserve
and the adaptive duration reserve are subtracted, and the archive gets the rest
as its own limit. The draw state and the duration histories are checked against
their reserves on every admission and fail the campaign when either exceeds one.
The archive's limit is enforced incrementally, a bounded number of eviction
visits per admission, so resident bytes sit above the limit while maintenance
catches up. Maintenance does not always converge below the limit: history
compaction batches and declines to run below `HISTORY_COMPACTION_MIN_DROPS`,
and entry dropping stops at one surviving active entry, so a campaign can
carry an over-budget tail of retained history to its end. Final compaction
bypasses the batching threshold and rejects an archive that is still over its
limit. Because of that lag, the archive's resident bytes are not checked
against the whole budget during the campaign.

## Workload boundary

`searcher` is independently buildable. Workload packages implement its typed
campaign and target contracts:

| Contract | Workload responsibility |
| --- | --- |
| `TargetExecution` | Construct, drive, restore, and snapshot targets; capture observations and account for deterministic execution work. |
| `InputPolicy` | Define the action vocabulary and the policy identifiers a recording must match. |
| `Evaluation` | Classify outcomes, derive archive keys, and accumulate progress and evidence. |
| `Reporting` | Identify and serialize recordings and assemble archive reports. |

An `ArchiveKey` answers three separate questions, and nothing else reads a
group as a magnitude:

| Question | Answered by |
| --- | --- |
| Is this a new place? | `Eq` on the group. `Ord` only lets maps store it. |
| Is this band, class or leaf further along? | `ArchiveKey::progress_cmp`, default `Ordering::Equal`. |
| Which of two states at one slot survives? | `ArchiveKey::preference_cmp`, default `Ordering::Equal`. |

`progress_cmp` must be a total preorder: any two groups compare, comparing them
in either order gives reversed results, and the relation is transitive over
every triple. The searcher relies on that to take a maximum in one indexed pass
instead of a dominance scan. `check_total_preorder` checks those properties over
a slice of groups; every workload that declares a `progress_cmp` calls it from a
test. A workload with no progress notion leaves the default, and its places are
then all peers.

Each contract depends on `CampaignTypes` and can be implemented independently.
A complete adapter receives the aggregate `Workload` implementation automatically.
The `tests/interfaces.rs` fixture implements execution alone and exercises it
through a function bounded only by `TargetExecution`.

`InputPolicy` requires four things of a workload: the action limit, the action
cost ceiling, the policy identifiers a recording must match, and
`sample_alphabet`, which draws one action from the workload's vocabulary. The
searcher supplies the rest. `expand_suffix` mixes `sample_alphabet` with a step
drawn from the retained-input table, `finish_stream_record` folds the record's
retained suffixes back into it, and `remember_draw_version` keeps the versions
a replay still names. A workload that embeds a duration choice in its actions
overrides `expand_duration_recorded_or_live` and draws through
`DrawTables::draw` itself. That one method serves the live and replay paths,
so a recorded run reaches the workload with the table version the stream
named.
`draw_table_parameters` sizes the table; its default reserve is 2 MiB and must
fit the campaign's memory budget.

Streams require the current engine `schema_version` in addition to the
workload's format identifier. Missing or unsupported engine versions are
rejected before replay. The stateful resource fixture uses a distinct checkpoint
shape and checks that corrupted checkpoint evidence is rejected.

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

Workload packages live in `../../workloads`. They own adapters, execution
integration, and campaign binaries. A probe must restore candidate state before
returning, including adapter caches and pending input.

`TargetExecution::execution_work` is a monotonic logical lifetime counter. It
starts at the workload's search genesis after setup, survives reset and
snapshot restore, and excludes probe work. The campaign records this measured
work separately from the declared per-action cost used by archive paths and
suffix bounds. Each workload declares both unit labels; the stream header and
report carry them, and replay rejects a workload whose identity or units do not
match. A work budget stops new admissions after the measured total reaches the
budget; already reserved jobs drain and can overshoot it. Physical cache or
backend counters remain workload diagnostics and never replace the logical
counter.

Run the core checks with:

```sh
cargo test --manifest-path dissonance/searcher/Cargo.toml
cargo clippy --manifest-path dissonance/searcher/Cargo.toml --all-targets -- -D warnings
```

## Adaptive duration policy

Campaigns can ask the generic searcher for a duration choice through
`InputPolicy::duration_request`. The workload supplies a typed context and a
positive maximum in its own stable logical unit. `expand_suffix_duration`
embeds the choice in actions, and `duration_of_action` identifies an action
that actually carried that choice. The searcher owns the policy and does not
interpret the unit or the context.

The policy keeps at most 256 contexts. Contexts enter a FIFO admission order
only when an applied duration receives positive execution work and the job is
admitted; all updates happen at ordered job admission.
Each retained context keeps only its most recent 128 observations. This keeps
memory bounded and lets old preferences expire. A suffix that does no work or
does not apply its selected duration produces no observation. The observation
is useful only when the action carrying that duration is itself retained or
reaches an objective. Retention caused by another action in the same suffix
does not give the duration credit.

If a parent is already in the failed execution state when a generic rollout is
prepared, the job result carries one preparation-failure observation vector and
no actions. Admission counts that result as one execution failure, gives the
observations to the workload's evaluation hook, and continues the campaign.
Preparation failures do not create candidates, objectives, or duration
observations. A nonfailed terminal parent still produces an empty result with
no preparation-failure report.

Choices are powers of two from one through the greatest power of two that fits
the requested bound. Half of draws explore scales uniformly. The other half
selects the greatest observed useful-outcome count per unit of logical work,
breaking ties with the seeded generator. With no useful observations both
halves explore. The exploration share and history sizes are fixed algorithm
constants, not workload knobs. Scores use integer cross-products and logical
work, never host timings. A maximum of one admits only a duration of one.
Changing duration or cost units requires a new workload policy identity and
history.

Ordinary non-splice jobs record the duration draw, the touched context history
at draw time, the admission sequence at which it was selected, and that
context's history immediately before and after ordered admission. Replay
reconstructs the recorded reservation work and context history from the
bounded admission window, verifies the choice against that state, replays the
recorded action expansion, and checks those bounded context checkpoints. It
does not serialize the complete context table for every job.
Full policy checkpoints contain the policy identity, FIFO context order, and
bounded histories; decoding rejects oversized context or observation arrays.
Deterministic continuation still requires the same seed, workload identity,
stable units, and ordered admission. The policy contains no wall-clock or
workload-specific vocabulary.

The unit tests cover bounded logarithmic draws, delayed outcomes, changing
phases, continued exploration, logical work comparisons, invalid observations,
and checkpoint decoding. The `tests/adaptive_campaign.rs` fixture runs the
generic policy through a two-worker campaign with short and long contexts,
ordered feedback, exact replay, and planted draw, checkpoint, and remaining
work changes that replay rejects. It does not measure a workload speedup.

## Adaptive barren horizons

Ordinary `OneToSix` suffixes extend after barren selection feedback. The
searcher keeps a persistent per-entry streak of admissions that retain no
descendant and appends a geometric deterministic alphabet horizon to the
ordinary suffix: streaks one, two, three, and four request one, two, four, and
eight actions. The extension is capped by the remaining action-limit capacity.
An entry that reaches ordinary retirement remains eligible only through this
bounded horizon retry window; after a failed full-capacity trial it returns to
the short uniform floor and future retries carry no extension. Selector
counter-window resets do not clear this streak; a retained descendant resets
it. The archive representative stays unchanged; the appended actions execute
and count as ordinary logical work, and replay reconstructs them from the
extension count recorded at reservation time. The streak is included in
archive selector counters so archive warm starts preserve the feedback.
`OneToSixBounded` deliberately
keeps its existing three-times-maximum-action-cost bound and receives no
extension.

The live archive counter does not change the campaign stream schema: each
reservation records its chosen extension, so existing streams remain exact
replay inputs. Older archive reports may omit the counter and import with a
zero warm-start streak; that is an archive warm start rather than a replay of
the prior selector history.

This is a fixed engine policy rather than a workload or runtime knob. It can
cross waits that produce no new key at the cost of longer rollouts from barren
parents, while the action limit bounds each rollout and the work counter charges
every appended action. The engine schema version identifies the semantics, and
the campaign schema is version 7. The generic regression fixture covers a
seven-step wait, a large distractor archive, a same-key cycle with charged work,
the bounded shape, archive import, and exact live/replay equality.

## Search evaluation policies

Five selector policies exist. `hierarchy_uniform_128_v2` draws uniformly over live
groups. `hierarchy_uniform_128_v2_retire:<thresholds>` drops a group once its
barren counter passes a threshold. The three `energy_frontier_cheapest`
identifiers weight the draw by barren energy, by progress rank, and by cost.

The coarsest group is a class. Every class holding a live cell receives draws.
The walk ranks classes by how many distinct progress levels are ahead of them,
capped at eight, and weights a class `1 << ((8 - rank) * 3)` times its barren
energy, so the leading class takes most of the draws, a class that stops
producing falls away, and no live class takes zero. The factor of eight per
rank is what keeps a deep run moving; a factor of two spreads the draws far
enough behind the frontier that a workload with many finished classes stops
finishing. Without the energy term a workload whose
classes form a chain of finished and unfinished stages spends half its draws
behind the frontier forever. The class depth is an ordinary pooled depth: the
`energy_frontier_cheapest` identifiers carry one scale per depth from the
finest pooled depth up to the class, and `group_barren` holds a counter at each
of them. `SelectorAccounting`'s `class_draws_by_rank` reports the share each
rank received. Bands inside a class are ranked the same way, over the distinct
progress levels of their frontier groups.

At pooled-group ties above the final cell, `hierarchy_uniform_128_v2` and the
energy variants use a persistent first-discovery ordinal for each live group. The
ordinal survives history compaction and archive import, so a replaced entry
cannot make an old route look newly discovered. Within each equal-progress peer
set at those depths, the oldest-to-newest ordinals multiply the existing weight by
`(peer_count + peer_rank) / peer_count`. The multiplier is in `[1, 2)`, so older
routes keep a useful bounded share even when there are fewer than eight peers,
and insertion order cannot erase them. Progress ordering and barren energy stay
primary because the multiplier is applied after their weight. Recency only breaks
equal-progress pooled-group ties. The final cell retains its existing eight-peer
novelty blocks, but uses the persistent ordinal so compaction and import do not
reset cell age.
Discovery ordinals are included in archive reports and cost their explicit `u64`
map storage in the memory accounting.

A productive selection clears the parent's barren counter at a pooled depth
only when a retained child's group at that depth had not been seen before. A
child that opens a new coarse group necessarily opens the finer groups
containing it, so it still clears every depth below. A child that is new only
at the finest pooled depth clears that depth alone, so a place that keeps
producing fine novelty inside ground the search already covers no longer holds
its coarser counters at zero. `Retire` clears every depth on any productive
selection; `hierarchy_uniform_128_v2` clears none. `SelectorAccounting` reports
`energy_resets`, the counters cleared at each depth, and `productive_by_mask`, a
histogram over productive selections of which depths the selection opened.

Every live progress line carries the whole of `SelectorAccounting` under
`selector`, so a run's class draw shares and energy resets can be read over
time rather than only from the final census.

Search experiments use independent versioned identifiers:

- `hierarchy_uniform_128_v2_energy_frontier_cheapest_count_v1:<thresholds>` divides
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

These are experiments, not new defaults. Promote policies based on paired workload
panels, fresh completion results, and resource costs through
[`benchmarks/search`](../../benchmarks/search/README.md). The generic resource
fixture exercises actual continuation dispatch, snapshot eviction, concurrent
reservations, exact report/checkpoint replay, and planted recording corruption
without a workload runtime or external artifact.

`hierarchy_uniform_128_v2_energy_frontier_cheapest_key_count_v1:<thresholds>` is a
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
improvements, which may reduce their ordinary draw share; the companion workload
panels must check that tradeoff. No default change is implied by the mechanism.

Progress sidecars carry objective workload evidence, actual admitted execution
work, terminal endpoint and execution-failure totals, final totals, logical
memory categories, and monotonic host time. With
`HARMONY_COORDINATOR_PROFILE=1`, they also contain coordinator phase durations
and dispatched replay/suffix action costs. Those costs are declared path cost,
not measured execution work. Profiling values and clocks never enter
search decisions or the deterministic campaign stream.

The last sidecar record of a run also carries `retained_diagnostics`, an
optional workload census over the archive's cached active endpoints. Each
endpoint is offered with the number of times the selector drew it, so a census
can separate a place the selector never went from one it went to and got nothing
from. The census reads only cached endpoints; a missing snapshot payload is
counted and never reconstructed, which makes every total a lower bound. It is
reporting only: it runs after the search is over and feeds nothing back into
selection, keys, rewards or the recorded stream.

Each recorded action carries an objective event and an execution disposition.
`Runnable` states can produce retained candidates even when the rollout stops
after observing an objective; `Terminal` and `Failed` states cannot. The rollout
stop flag controls that suffix, while the campaign stop flag controls new
reservations. A latched objective inherited from a retained parent is not
reported again, so continued suffixes can still be evaluated without duplicate
objective counts.
Archive-origin campaigns preserve workload evidence while objective totals and
witnesses count objectives evaluated during the new campaign.
Genesis and snapshot-root bootstrap still require a current key and retained
snapshot; a terminal target without a snapshot is reported as an execution
error.

`run_campaign_checkpointed_with_options` accepts an optional deterministic work
budget without changing existing `CampaignConfig` callers. The stream and
report record that budget only when present. Already reserved jobs drain
normally; evaluators must score first-objective work against the threshold and
account for any drained overshoot. Omitting the option leaves the campaign
without a work-budget cutoff.
