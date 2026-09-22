<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# searcher

`searcher` implements deterministic search independently of a workload. The
`search::` modules own archive retention, parent selection, input mutation,
campaign coordination, worker execution, seeded draws, checkpoints, stream
recording, and replay. Workloads supply associated types through `CampaignTypes`
and implement four contracts. `Workload` composes those contracts for a full campaign.

The archive keeps entries in cells, a cell being one place at one progress
level, and ranks the cells in progress tiers. A workload provides the key, a
progress order and an ordered list of same-place state preferences; the generic
archive uses only those and retains bounded representatives. Campaigns reserve jobs
in a deterministic admission window, allow physical workers to execute them,
and process results in recorded admission order. The stream records the
configuration, policies, origins, jobs, admissions, skips, and progress needed
for replay. Reserved jobs pin the snapshot they actually restore, including a
parent's keyframe. If retention removes that snapshot from the active population,
its memory stays charged until the last reservation is admitted. Live execution
and serial replay release these pins at the same recorded boundary, independent
of worker completion timing. A memory-bounded archive keeps a liveness anchor,
a resident entry that stays available as a parent. Maintenance evicts toward
the archive's memory limit but keeps pinned snapshots, the liveness anchor, and
fixed reserves, so a budget smaller than that working set leaves the archive
above its limit. The archive picks the anchor, and reactivates it when no entry
is expandable, during the maintenance after bootstrap and after each admission
and skip. Parent selection does not change the archive, so replay reaches the
same archive state without repeating selection. Campaign streams require
schedule policy version 3
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

An `ArchiveKey` names three things about a state, and nothing else reads a
key as a magnitude:

| Question | Answered by |
| --- | --- |
| Where is this state? | `ArchiveKey::place`. A place is compared with `Eq`; `Ord` only lets maps store it. |
| How far along is it? | `ArchiveKey::progress`, an `Ord` value. `()` for a workload with no progress notion. |
| Which same-place states are distinct? | `ArchiveKey::identity`. Two entries at one place and progress with different identities hold separate slots. |
| Which states at one slot survive? | `ArchiveKey::preference_cmp` at each of `ArchiveKey::preferences()` indices; the default declares none and compares `Ordering::Equal`. |

A cell is a progress value paired with a place. A slot is a cell paired with
an identity. A slot keeps the top `capacity()` entries under each preference,
and its contents are the union of those sets. A candidate enters when it
reaches that set under any one preference; an entry leaves when it holds a
place under none. One entry can hold a place under several preferences and is
stored once. The stream header carries `preference_portfolio`, and a recording whose
portfolio differs from the compiled key is rejected. `selector.portfolio`
reports the preference count, holders held under one preference and under
several, replacements per preference, and admissions that improved
more than one preference at once.

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
The context table must fit a fixed reserve that the archive's memory budget
excludes. The coordinator checks the table against that reserve after each
admission.
Deterministic continuation still requires the same seed, workload identity,
stable units, and ordered admission. The policy contains no wall-clock or
workload-specific vocabulary.

The unit tests cover bounded logarithmic draws, delayed outcomes, changing
phases, continued exploration, logical work comparisons, invalid observations,
and checkpoint decoding. The `tests/adaptive_campaign.rs` fixture runs the
generic policy through a two-worker campaign with short and long contexts,
ordered feedback, exact replay, and planted draw, checkpoint, and remaining
work changes that replay rejects. It does not measure a workload speedup.

## Search evaluation policies

One selector exists, `tier_cell_count_decay_v1`, and the stream header names
it as `parent_scheduler`. A draw walks three levels. The tiers are the distinct
progress values held by selectable entries, ranked from the deepest; a tier at
rank `r` weighs `1 << ((8 - min(r, 8)) * 3)`, so the leading tier takes most of
the draws, and no tier holding an entry takes zero. Within the tier each cell
weighs `1 / (1 + draws)` over the draws it has received since it was last
reset, so an untried cell outweighs a heavily sampled one and every cell keeps
a share. Within the cell each holder weighs `1 / (1 + selections)` over its
own selection count. There is no uniform path, no sampling window and no
retirement: a cell that stops producing keeps drawing at a share that only
shrinks with its count.

A cell's draw count resets to zero when an arrival displaces a holder it
strictly outranks under a preference, so a place reached again with more of
what the preference counts draws like a place reached for the first time.
`SelectorAccounting` reports `cell_selections`, `productive_selections`,
`cell_resets`, `tier_draws_by_rank` and the draws each cell received, and
every live progress line carries it under `selector`.

Continuation replay carries a better state at one place to the places reached
from it. An exit source is a place paired with an identity, so two holders that
differ only in what they carry share one set of exits. The archive keeps one
edge per exit source and destination place holding the cheapest action tail
observed between them, the donor and leaf it came from, and the preferences the
tail gained from its source to its arrival. When a replacement wins its slot
under `preference_cmp` with `Ordering::Greater`, its exit source is queued at
the index of the lowest preference it took. A reservation that takes the queue
examines at most 8 exits, skipping stale parents, prefixes already archived and
parents at the action limit. Among the rest it dispatches the first whose
holder beats every current holder of the destination place under the
preference it won, and otherwise the first edge that gains a preference; an
edge that does neither is skipped. Landing is arrival anywhere in the
destination place; acceptance there is the ordinary slot rule after replay. A
result that lands and wins there queues its own source in turn; that chain is a
wave, and `longest_wave` reports the deepest one.

The queue holds one entry per exit source, not per edge, ordered by preference
index and then by arrival. Queuing a source is two map operations whatever its
degree, and a source queued again under a lower preference index moves to that
tier keeping its place within it. A pop takes the next exit after the front
source's cursor, advances the cursor and moves the source to the back of its
own tier, so sources rotate and a source improved on every reservation cannot
hold the front. One pop in four takes the highest tier present instead of the
lowest, so a preference that improves rarely still propagates. A source whose
exits run out leaves the queue, and removing a source or a place releases its
edges and the pending entries that depended on them.

One reservation in four attempts a continuation while the queue is not empty.
The share is fixed rather than fed back from how the replays are doing: one in
eight starves the chain and one in two crowds out ordinary exploration. The
draw is `RomuDuoJrRand::with_seed(campaign_seed ^ reservation)`, so replay
recomputes it at each reconstructed reservation and rejects a record whose
`continuation_energy` disagrees; the tier draw salts the same seed.

The bank exists only for a workload whose key declares a preference. Without
one nothing is recorded, nothing is charged, and the stream is unchanged.
Edges and pending entries are charged as they are held rather than reserved up
front, and compaction drops the edges of sources and places the archive no
longer holds. Dispatch records the complete action tail, so later donor
reclamation cannot change serial replay. Only same-place `preference_cmp` is
consulted; preferences are never compared between unrelated places.

`ContinuationAccounting` rides every live progress line under `continuations`:
`edges` and `pending` for the bank's size, `jobs`, `execution_work`, `landed`,
`replaced`, `opened_new_cell` and `useful` for what the replays did, where a
useful landing is one whose entry later bred a retained child,
`gaining_dispatched` for edges dispatched on their gain alone, `longest_wave`,
and `energy`, `reservations_drawn` and `reservations_taken` for the share.

Search experiments are promoted through paired workload panels, fresh
completion results, and resource costs in
[`benchmarks/search`](../../benchmarks/search/README.md). The generic resource
fixture exercises actual continuation dispatch, snapshot eviction, concurrent
reservations, exact report/checkpoint replay, and planted recording corruption
without a workload runtime or external artifact.

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

