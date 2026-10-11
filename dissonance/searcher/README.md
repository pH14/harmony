<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# searcher

`searcher` implements deterministic search independently of a workload. The
`search::` modules own archive retention, parent selection, input mutation,
campaign coordination, worker execution, seeded draws, checkpoints, stream
recording, and replay. Workloads supply associated types through `CampaignTypes`
and implement four contracts. `Workload` composes those contracts for a full campaign.

The archive keeps entries in cells, a cell being one place at one progress
value, and ranks the cells in progress tiers. A workload provides the key, a
progress order and an ordered list of state preferences; the generic
archive uses only those and retains bounded representatives. Campaigns reserve jobs
in a deterministic admission window, allow physical workers to execute them,
and process results in recorded admission order. The window is the number of
reservations in flight and is set apart from the worker count. Each job's
planned finish is the planned finish of the last admission before its
selection plus `action_cost` summed over its replay and suffix actions. Jobs
are dispatched and admitted in planned-finish order, with ties broken by
reservation, so a short job reserved after a long one does not wait for it.
Job records carry their reservation, and replay checks that admissions follow
planned-finish order and that reservations form a contiguous sequence. One coordinator
random generator draws every selection, so a fixed seed and window write the
same stream at any worker count. Worker ids appear only in run telemetry. The stream records the
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
same archive state without repeating selection. The final report selects no
more parents, so its compaction releases metadata pins and the liveness anchor
and then requires the compacted archive to fit the memory limit. Campaign streams require
schedule policy version 5
and the current bounded progress policy; recordings from superseded policy
namespaces are rejected before replay because their snapshot accounting differs.

Empirical step tables fold retained suffixes into an incremental hash and a
deterministic frequency map capped at 4,096 distinct steps. The compact table
is the only supported representation. Every campaign keeps one, in
`search::draw_tables`, so a workload gets the biased draw without owning any of
the bookkeeping. `DrawTables` folds each retained suffix as its record closes,
publishes an `EmpiricalStepCheckpoint` the stream records beside every draw,
and keeps the table versions a serial replay still needs. The tables also hold
workload feedback: weights keyed by `u64` that a workload computes from its
campaign evidence. Feedback changes only at a table update, enters the table
hash when nonempty, keeps at most the 4,096 heaviest keys, and reaches each draw
through `DrawView` with the table version the draw names. Workloads identify
their input policies and reject unknown or retired identifiers during replay.

The worker pool starts before the bootstrap. The coordinator bootstraps while
every worker but the first builds its target, and the first worker builds its
target after the bootstrap target is dropped, so a campaign never holds more
targets than workers. The first worker's boot time includes that wait.

Workers pull jobs from one shared queue ordered by planned finish, so an idle
worker takes the queued job that admission reaches first while another worker
is still busy. A short job reserved after longer ones runs before them,
because admission needs its result first. The coordinator dispatches each job
when it reserves it, so the admission window alone limits the jobs that are
queued, running or finished but not yet admitted. Memory held by finished
results is outside the archive's logical budget and must be measured in host
RSS. Telemetry charges idle worker time to the pool: `idle_admission_order_ns`
when finished results wait on an earlier job and `idle_no_job_ns` otherwise.
A wall-time stop, unlike a fixed work ceiling, can change with execution speed.

`memory_budget_mib` is split before bootstrap: the workload's draw-state reserve
and the adaptive duration reserve are subtracted, and the archive gets the rest
as its own limit. The draw state and the duration histories are checked against
their reserves on every admission and fail the campaign when either exceeds one.
The archive's limit is enforced incrementally, a bounded number of eviction
visits per admission, so resident bytes sit above the limit while maintenance
catches up. Maintenance does not always converge below the limit: history
compaction batches and declines to run below `HISTORY_COMPACTION_MIN_DROPS` or
one sixteenth of the entries, whichever is larger, and entry dropping stops at
one surviving active entry, so a campaign can carry an over-budget tail of
retained history to its end. A scan that finds too few entries to drop records
how many it found. Later scans wait until deactivations, released pins, a moved
liveness anchor and reclaimed snapshots could have freed enough entries to reach
the threshold, so an archive whose history stays over its target scans once per
batch of releases instead of once per admission. A skipped scan could not have
compacted, so the stream is unchanged. A compaction rebuilds the slot, selector
and input indexes in time proportional to the entries, so the batch grows with
the archive. An archive at its active-entry cap retires one entry per admission
and compacts once every sixteenth of its size in admissions, which keeps rebuild
time per admission constant. Final compaction bypasses the batching threshold
and rejects an archive that is still over its limit. Because of that lag, the
archive's resident bytes are not checked against the whole budget during the
campaign.

Prefix-tree compaction remaps a single surviving child in its existing map.
Branching maps are rebuilt so compaction still releases their unused storage;
missing children follow the same rebuilding path. This preserves node order,
parent links, owners, and serialized state while avoiding a map allocation and
free for each retained single-child node. The opt-in
`DISSONANCE_BENCHMARK_PREFIX_COMPACTION=1` unit benchmark compares this path
with rebuilding every map in the same binary. Its timings cover prefix-tree
compaction, not whole-search throughput.

Compaction also removes stale continuation-landing markers by walking their
sorted IDs together with the rebuilt entry-ID index. It does not build another
set of every live ID or perform a tree lookup for each marker. The retained
markers and logical memory accounting are unchanged.
`DISSONANCE_BENCHMARK_LANDINGS_CLEANUP=1` enables paired measurements of this
cleanup against the original temporary-set implementation.

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
more than one preference at once. Portfolio reporting stops at the counts needed
to classify a holder: `capacity()` better entries disqualify a preference, and
two won preferences establish a shared holder. A slot with at most its capacity
of entries needs no preference comparisons. These shortcuts change only reporting
work, not admission rankings or the reported exclusive/shared counts.

For a paired comparison with the original full-count reporting implementation:

```sh
DISSONANCE_BENCHMARK_PORTFOLIO=1 cargo test --locked --manifest-path dissonance/Cargo.toml --release --lib portfolio_classification_matches_reference_fixtures -- --nocapture
```

The paired measurement alternates baseline/candidate/candidate/baseline blocks
and their inverse to reduce sensitivity to concurrent host load. Without the
environment variable, this test checks the same fixtures without timing them.

Each contract depends on `CampaignTypes` and can be implemented independently.
A complete adapter receives the aggregate `Workload` implementation automatically.
The `tests/interfaces.rs` fixture implements execution alone and exercises it
through a function bounded only by `TargetExecution`.

`InputPolicy` requires two things of a workload: the policy identifiers a
recording must match, and `sample_alphabet`, which draws one action from the
workload's vocabulary. The
draw receives the action just before it: the previous action of the suffix, or
the parent's last action for the first one, so a workload can draw a change to
what the input already holds. The searcher supplies the rest. `expand_suffix` mixes `sample_alphabet` with a step
drawn from the retained-input table, `finish_stream_record` folds the record's
retained suffixes back into it and receives the campaign evidence after the
record's admission, so a workload can pass feedback to
`DrawTables::finish_record_with_feedback`, and `remember_draw_version` keeps the versions
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

The shared `search::rollout` loop owns action evidence capture, candidate
creation, retention probe placement, and stopping; a workload provides action execution and state evaluation.

Workload packages live in `../../workloads`. They own adapters, execution
integration, and campaign binaries. A probe must restore candidate state before
returning, including adapter caches and pending input.

`TargetExecution::execution_work` is a monotonic logical lifetime counter. It
starts at the workload's search genesis after setup, survives reset and
snapshot restore, and excludes probe work. The campaign records this measured
work separately from the declared per-action cost used by archive paths and
planned finishes. Each workload declares both unit labels; the stream header and
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

Archive admission visits the same sorted preference winners to build holder
retention flags and the candidate's winning preferences directly. It avoids
allocating preference lists for existing holders. Accepted suffixes transfer
their allocation into the archive; excess capacity is removed before storage.
`ArchiveCandidate` takes either owned or borrowed suffix storage. Campaign
admission borrows its pending actions, so rejected and duplicate candidates do
not copy them. Retained borrowed suffixes become exactly sized owned vectors.
Admission ranks against the borrowed holder list. Only accepted candidates copy
that list and inherit and record their lineage. Rejection does not allocate
metadata that would immediately be discarded.
These allocation changes preserve ranking, tie breaks, and logical memory charges.
Archive reports stream borrowed input suffixes and milestones through the
serializer; they keep the same wire fields and ordering without an intermediate
vector of owned entries.
The microbenchmarks cover rejection and acceptance with short and long suffixes,
plus serialization of reports with independent roots and shared prefixes:

```sh
cargo bench --locked --manifest-path dissonance/Cargo.toml --bench archive_admission
cargo bench --locked --manifest-path dissonance/Cargo.toml --bench duration
cargo bench --locked --manifest-path dissonance/Cargo.toml --bench parent_selection
```

Each tier keeps its cells, and each cell keeps its holders, in a treap (a
binary search tree kept balanced by pseudo-random node priorities) ordered by
key, where each node stores the count-decay weight sums of its subtree and of
its left subtree. A draw descends to the first key whose prefix sum exceeds the
drawn value, which is the key a linear prefix scan over the sorted keys
returns, so draws and RNG consumption match the linear draw. Count-decay
weights are at most 2^32 and the archive holds at most 2^22 active entries, so
every subtree sum is at most 2^54 and fits in a `u64`. Draws, weight
updates, inserts at any position, and removals cost expected O(log n) in the
number of cells or holders. Each cell also keeps its holders in one ordered set
per preference, so finding, adding, or removing the best holder costs O(log n).
This requires `ArchiveKey::preference_cmp` to be a total order among keys that
share a place. Checkpoints store the same key sets as before, and the weights
and ordered sets rebuild on the first selection after loading. Debug builds
check every drawn tier's weights and best holders against a fresh computation.

The candidate-draw differential tests compare IDs, errors, and exact RNG state
with the previous implementation. The opt-in paired benchmark includes isolated
cell/holder draws and complete parent selection with zero-sized place keys:

```sh
DISSONANCE_BENCHMARK_CANDIDATES=1 cargo test --locked --manifest-path dissonance/Cargo.toml --release --lib indexed_candidate -- --nocapture --test-threads=1
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
An admitted observation updates an existing context through one map lookup;
a new context follows the same FIFO eviction and history allocation rules.
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

Tier selection iterates rank weights and reads the selected progress value from
the ordered tier map. This avoids two temporary vectors per parent selection;
weights, traversal order, saturating totals, and RNG consumption stay identical.

One selector exists, `tier_pace_yield_cell_recent_count_decay_v1`, and the stream header names
it as `parent_scheduler`. A draw walks three levels. The tiers are the distinct
progress values held by selectable entries, ranked from the deepest; a tier at
rank `r` weighs `1 << ((8 - min(r, 8)) * shift)`, where the key's
`tier_rank_shift` is three unless the workload says otherwise, so the leading
tier takes most of the draws, and no tier holding an entry takes zero. The
leading tier gives up weight while it finds nothing new. Each tier counts its
draws since a new cell last appeared in it and keeps the longest such run; its
pace is that longest run or its cell count, whichever is larger. When the
current count passes twice the pace, the leading tier's weight halves, and it
halves again each time the count doubles, down to the weight of the rank
below. The halvings apply only while the rank below yields more per draw than
the leading tier since the leading tier's last new cell. A tier's yields are
its new cells and its carried-in wins, admissions that take a slot's
preference from a parent in another cell. The leading tier counts only its
carried-in wins there, since a new cell ends the run. With `Td` and `Ty` the
leading tier's draws and wins in the run, and `Nd` and `Ny` the draws and
yields of the rank below over the same span, the halvings apply while
`(Ny + 1) * (Td + 1) > (Ty + 1) * (Nd + 1)`. When the rank below is a
different tier from the one recorded when the run began, or no tier was
recorded, `Nd` and `Ny` are that tier's total draws and yields. A leading tier
that yields at least as much per draw as the rank below keeps its full weight. A new cell in
the tier restores the full weight. A
workload whose progress order has many close steps, such as fine progress
bands, supplies a shift of one so each rank takes half of the one ahead. The
largest accepted shift is seven, because a larger one overflows the leading
tier's 64-bit weight; a draw under a larger shift fails with an error. Within the tier each cell
weighs `1 / (1 + draws)^2` over the draws it has received since it was last
reset, so an untried or freshly reset cell takes most of the tier's draws
until it catches up and every cell keeps a share. Within the cell, a quarter
of the draws go to the cell's best holder under each preference, split evenly
across the key's preferences; ties go to the lower cost and then the older
entry, and a holder best under several preferences takes each of their parts.
This keeps a cell's best-stocked states drawn often when the cell holds many
holders, so searches that leave the cell start from them. The other draws weigh each holder
`1 / (1 + selections)^2` over its own selection count, and a key without
preferences draws every holder that way. Half of tier draws first try a recent-arrival window: the newest 256
selectable entries, deduplicated by cell, limited to the drawn tier and cells
with fewer than 32 draws since their last productive reset. These cells use
the same count weights and holder selection. An empty window falls back to
the full tier; the other half always uses the full tier. The window derives
from the existing ordered active index and saved counts, so it adds no
checkpoint state. `recent_selections` records draws through this path. There
is no retirement: a cell that stops producing keeps drawing at a share that only
shrinks with its count.

A cell's draw count resets to zero when an arrival from another cell
displaces a holder it strictly outranks under a preference, so a place
reached again with more of what the preference counts draws like a place
reached for the first time; an improvement whose parent sits in the same
cell, such as a resource gained by repeating an action in one place, leaves
the count alone. It
also resets when a selection from the cell opens a cell that held nothing, so
the cells at the edge of explored ground keep drawing while they keep opening
new ground instead of settling to an equal share with every cell behind them.
`SelectorAccounting` reports `cell_selections`, `productive_selections`,
`cell_resets`, `tier_draws_by_rank`, `best_holder_draws` per preference,
`tier_runs` keyed by each tier's progress value with its current and longest
run, its total draws and yields, its carried-in wins in the current run, and
the rank below's name, draws and yields when the run began, and the draws each
cell received, and
every live progress line carries it under `selector`. The draws each cell
received and `selector.portfolio` appear once each time the executions double,
on the line at 102,400 executions times a power of two, and on the final line.
Counting portfolio holders compares every pair of holders in each slot, which
on every line would take most of the coordinator's time. The draws of hundreds
of thousands of cells take tens of megabytes per line.

Continuation edges copy their action tail only after the existing-cost check
accepts the edge; equal-cost and more expensive routes leave the bank unchanged
without allocating an action vector.
Pending-source updates use one tree-entry lookup, reusing the vacant position
when queuing a source for the first time. Existing-source refreshes keep their
sequence and update the same fields; a preference change moves the same queue
entry. This changes neither dispatch order nor serialized state or memory charges.

The pending-entry differential test compares serialized bank state and dispatches
through mixed recording, queuing, popping, removal, and saturated-counter cases.
An opt-in paired benchmark compares the original and single-lookup queue updates
in the same release binary, alternating ABBA/BAAB timing blocks:

```sh
DISSONANCE_BENCHMARK_PENDING_ENTRY=1 cargo test --locked --manifest-path dissonance/Cargo.toml --release --lib pending_entry -- --nocapture --test-threads=1
```

Modes 0–3 measure new-source insertion, reprioritization, same-preference refresh,
and the no-outgoing-edge control respectively.

The energy mixtures choose among three input strategies: the retained-input
table, the alphabet, and a splice, which appends to the parent the recorded
route from another holder of the parent's slot to that holder's deepest
retained descendant, up to 128 actions. The donor shares the parent's slot
because a route only reproduces its moves from where it was recorded. A draw
with no such donor runs as an ordinary draw. Each strategy's share halves for every `scale` average jobs'
worth of execution work it has spent since its last job that opened a new
slot, so a strategy is judged on new slots per unit of work and a long
splice that opens nothing loses its share sooner than a short draw. The live
progress line counts `splice_jobs`, `splice_actions` and `splice_cost` under
`coordinator`.

The donor is the holder of the parent's slot whose deepest descendant ranks
highest, read from the slot's active holders when the splice is prepared. Each
admission records itself as the deepest descendant of its ancestors until an
ancestor already holds a deeper one, and that walk follows parent positions
stored beside the entries.

The archive builds each entry's input node by extending its parent's node with
the entry's suffix, so a donor that owns its input node has an intact prefix of
its recorded length. Checkpoint restore checks that length once for every entry
that owns its node, and splice preparation then trusts such a donor. A donor
that does not own its node has its complete prefix validated without copying
it. Preparation then walks the leaf's suffix and copies only the requested
leading tail. Reaching the same prefix node after the declared
suffix length reuses the donor validation. If prefix nodes differ, their actions
are compared while validating the leaf prefix, so equivalent noncanonical paths
and invalid-path error precedence retain their previous behavior. Preparation
allocates only the returned tail; archive state and selection policy are unchanged.
Declared path lengths are bounded by the number of stored non-root node slots,
so malformed lengths cannot turn a parent cycle into an unbounded validation walk.

The splice tests compare bounded reconstruction with the original full-input
reconstruction, including zero limits and malformed metadata. A clone-count test
checks that the archive copies only returned actions. Run the same-binary paired
benchmark with:

```sh
DISSONANCE_BENCHMARK_SPLICE_TAIL=1 cargo test --locked --manifest-path dissonance/Cargo.toml --release --lib bounded_splice -- --nocapture --test-threads=1
```

Each drawn suffix takes its length from its parent's earlier jobs. A stretch
where a key's place and
preferences stay fixed can only be crossed by a single job, because every state
inside it ties with or loses to the state that arrived there first. A parent's
first job runs one action. After a job that kept no state, never left the
parent's place and did not end in a terminal state, the parent's next length is
twice the longest such job since the last reset, up to 64 actions. A job that
kept a state or left the place resets it to one action. A job that
ended in a terminal state without either leaves it unchanged. The archive holds
the length per entry, so checkpoints carry it and compaction drops it with its
entry. Splices and continuations run their recorded tails and leave it
unchanged. The coordinator reads the length when it dispatches the job and
records it as `suffix_limit` in the job or skip record. While a job of that
length or longer from the same parent is still in flight, the next job runs one
action instead. The job in flight already tests that length, and admission in
planned-finish order returns long jobs late, so more jobs of that length would
repeat the same test.
Replay cuts the redrawn suffix to the recorded limit. It rejects a drawn job
whose limit is missing or outside 1 to 64, and a spliced or continued job that
records a limit.

Continuation replay carries a better state at one position to the positions
reached from it. A position is a place paired with an identity, the `Position`
type, so two holders that differ only in what they carry share one set of
exits. Every retained parent and child whose positions differ records an edge
between the two positions, inside one place or across two, holding the
cheapest action tail observed between them, the donor and leaf it came from,
and the preferences the tail gained from its source to its arrival. Edges
inside a place give nearly every position on a route an exit, so an
improvement anywhere queues. When a replacement wins its slot under
`preference_cmp` with `Ordering::Greater`, its position is queued at the index
of the lowest preference it took. A reservation that takes the queue examines
at most 8 exits, skipping stale parents and prefixes already archived. Among the rest it dispatches the first whose holder beats
every current holder of the slot it would land in, the parent's progress at the
destination position, under the preference it won, and otherwise the first edge
that gains a preference; an edge that does neither is skipped. An edge inside
one place lands only on its destination position; an edge into another place
lands anywhere in that place, and the arrival's own position is what queues
next. Acceptance there is the ordinary slot rule after replay. Replay runs one
edge per job, so each hop's landing check stops divergence from compounding. A
result that lands and wins there queues its own position in turn; that chain is
a wave, and `longest_wave` reports the deepest one.

The queue holds one entry per position, not per edge, ordered by preference
index and then by arrival. Queuing a source is two map operations whatever its
degree. A source queued again takes the parent and the preference index of its
latest improvement and keeps its arrival order, so the check before replay
compares the parent on the preference it won. A pop takes the next exit after
the front source's cursor, advances the cursor and moves the source to the back
of its preference index, so sources rotate and a source improved on every
reservation cannot hold the front. One pop in four takes the highest preference
index present instead of the lowest, so a preference that improves rarely still
propagates. A source whose
exits run out leaves the queue, and removing a position releases its
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
front, and compaction drops the edges of positions the archive no longer
holds. Dispatch records the complete action tail, so later donor
reclamation cannot change serial replay. The check before dispatch compares
the parent's key at the source position with the holders of the destination
slot under `preference_cmp`, and for an edge into another place those keys sit
in different places. This assumes a workload's preferences compare the same way
in every place.

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

The live progress sidecar gets a line at the first execution and every 100
executions until 204,800. After that the step between lines doubles each time
the executions double, so each doubling adds 1,024 lines, the same bound the
progress curve keeps. A 250-million-execution run writes about 12,500 lines.
The cadence depends only on the execution count, so a resumed run writes the
same lines as an uninterrupted one.

Progress sidecars carry objective workload evidence, actual admitted execution
work, terminal endpoint and execution-failure totals, final totals, logical
memory categories, and monotonic host time. With
`HARMONY_COORDINATOR_PROFILE=1`, they also contain coordinator phase durations
and dispatched replay/suffix action costs. Those costs are declared path cost,
not measured execution work. The same lines carry `host_times`: the coordinator
thread's CPU and run-queue time from Linux scheduler statistics, and worker idle
time split into waiting for admission order and waiting for a job, counted from
the start of the search in this process. Phase durations are wall time and
include time the coordinator thread waits for a CPU, so coordinator work per
job is `coordinator_cpu_ns` over admissions. Profiling values and clocks never enter
search decisions or the deterministic campaign stream.

The campaign report carries `telemetry`, the host measurements that explain
where a run's time went. It records bootstrap, search, and persistence wall
time; the coordinator phase durations; and the coordinator's receive time,
split into time when every worker was running and time when finished results
waited for an earlier result's admission. It also records how many results
were held for admission order and for how long. Each worker record holds boot,
busy, and idle time, with idle time split into waiting for admission order and
waiting for a job. It also holds jobs run, Linux scheduler CPU and run-queue
wait time, and the workload's own counters from `TargetExecution::telemetry`,
which the report also sums across workers. Report equality and the report's
serialized form leave telemetry out, so a replayed report stays byte-identical;
a caller that wants it writes it separately.

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

`CampaignExecutionOptions::placement` is a `ThreadPlacement` hook the campaign
calls once on the coordinator thread and once on each worker thread before
that worker builds its target. A launcher uses it to pin threads to cores. A
failed placement fails the campaign with its message. Placement changes only
where threads run, so a placed campaign records the same stream as an unplaced
one.

`run_campaign_checkpointed_with_options` accepts an optional deterministic work
budget without changing existing `CampaignConfig` callers. The stream and
report record that budget only when present. Already reserved jobs drain
normally; evaluators must score first-objective work against the threshold and
account for any drained overshoot. Omitting the option leaves the campaign
without a work-budget cutoff.

## Search checkpoints

`CampaignExecutionOptions::checkpoints` writes the whole search state during a
run: the coordinator counters, the archive with its cells, lineages and
continuation queue, the workload evidence, the draw tables, the adaptive
duration policies, the worker random states, and every reserved job that is not
yet admitted. A checkpoint is written after an admission and the selection that
follows it, so resuming re-executes the unadmitted jobs and admits them in the
same order. `CheckpointPlan` writes one at a fixed execution interval, at each
new workload milestone (`Reporting::checkpoint_marks`), and at each new top
archive tier. A final checkpoint is also written after the reservation queue drains,
including for runs shorter than the periodic interval. Milestone and tier
checkpoints are kept; only the last two interval checkpoints are kept.

Snapshots go into one append-only `snapshots.store` per directory. An archive
entry's snapshot never changes, so each is written once and later checkpoints
list it by entry id and offset. The writer counts the kept checkpoints that
list each stored snapshot. Deleting an interval checkpoint decrements the
counts of the snapshots it listed. A snapshot no kept checkpoint lists leaves
the writer's map, and on Linux its byte range in the store becomes a hole
(`fallocate` with `FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE`). The file
keeps its length and every other offset stays valid, so no checkpoint file
changes. The writer syncs the directory after deleting a pruned checkpoint and
before punching, so a crash cannot bring back a checkpoint whose snapshots are
gone. On macOS, and on a Linux filesystem without hole punching, the freed
bytes stay in the file. Each `.ckpt` file holds its header, that index, and
the postcard body. `checkpoints.jsonl` records write time and sizes:
`store_bytes` is the store's length, `live_snapshot_bytes` is the bytes that
kept checkpoints list, and `freed_snapshots` and `freed_snapshot_bytes` count
what the write's pruning released. The
header names the body's layout, `SEARCH_CHECKPOINT_FORMAT`. The name changes
whenever a stored type such as `Archive` changes, and a reader refuses a
checkpoint of any other layout before it decodes the body. The
archive stores its `SelectorAccounting` as JSON inside that body, so a counter
added there, such as `tier_runs`, reads as empty from a checkpoint written
before the counter existed.
`CampaignOrigin::SearchCheckpoint` resumes one. The admission
window, limits, workload identity and the workload policies that give stored
inputs and keys their meaning must match. The mixture and retention
policies, the selector, the continuation policy and the objective stop may
change, so a search can continue under a revised algorithm. The draw table
policy, the preference portfolio and a workload's `preference_policy` may also
change, because their state is rebuilt from the archive entries: new draw
tables fold every entry's suffix, and each slot re-ranks its holders under the
new preference order and capacity. The origin record and stream header list
each change as `checkpoint_policy_changes`. The same seed
repeats the original progress lines; another seed derives a new selection random
state and keeps everything else. The draw tables continue their table hash
from the recorded one, so stream draw-table hashes after a resume differ from an
uninterrupted run. A resumed stream cannot be replayed. The checkpoint stores
every reservation between the next admission and the last reservation, whether
queued, running or finished, and the resumed run dispatches them again, so it
may use a different worker count. Workloads opt in through
`Reporting::evidence_checkpoint` and `Reporting::evidence_from_checkpoint`.


## Host hashing

On native ARM64 Linux and macOS, SHA-256 uses runtime-detected CPU acceleration,
including when this component is built independently. Other targets and Miri
retain the existing backend selection. Hash inputs and outputs are unchanged.
The [host SHA qualification](../../scripts/qualification/README.md) checks independent builds and compares real consumers with software hashing.

`checkpoint::read_header` exposes the recorded scheduling window to adapters
that resume on a different worker count. Reusing that window preserves admission
ordering; the checkpoint reader still validates execution identity and policies.

The checkpoint journal records both completed (`executions`) and reserved
(`reserved`) work. A consumer granting an additional execution budget starts
from `reserved`, because restoring the checkpoint also restores already queued
work. At a completed checkpoint those counts are equal.
