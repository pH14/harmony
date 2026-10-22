<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# searcher

`searcher` implements deterministic search independently of a workload. The
`search::` modules own archive retention, parent selection, input mutation,
campaign coordination, worker execution, seeded draws, checkpoints, stream
recording, and replay. Workloads may implement `TargetExecution::verify_integrity`
to withdraw the entire execution domain after a determinism contradiction.
Execution, reservation, admission and final report boundaries check this shared
status. Its error aborts the campaign without an End record or completed report;
it is not an ordinary failed target, and coordinator checks preserve the typed
error independently of worker diagnostic strings. Workloads supply associated types through `CampaignTypes`
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
planned-finish order and that reservations form a contiguous sequence within
the recorded execution budget. One coordinator
random generator draws every selection, so a fixed seed and window write the
same stream at any worker count. Worker ids appear only in run telemetry. The stream records the
configuration, policies, origins, jobs, admissions, skips, and progress needed
for replay.

Physical assignment chooses the earliest queued planned-finish/reservation key
first, then the best currently idle worker for that job's optional opaque
locality key. Workloads advertise at most 64 keys per idle worker and assign a
static rank to each match. A matching worker outranks a nonmatching worker;
higher ranks win, followed by the smaller worker identifier. The queue never
waits for a busy owner or bypasses its head for a later local job. Advertisements
are removed on assignment or worker exit and rebuilt after execution or target
recovery. They convey no ownership or integrity authority. Selection, seeds,
planned finishes, checkpoint state, retention and admission remain independent
of these hints. Workloads without locality keys use the same queue with empty
advertisements.

A deterministic two-worker campaign fixture exercises initial and refill
plan drains with an owned decoded-snapshot cache. It waits for actual queue idle
membership, starts with the parent retained only by worker 1, and compares empty
and populated advertisements. Cache misses decode and allocate the restored
value; hits restore from the retained value. It checks avoided reconstruction,
busy-owner fallback, and byte-identical complete logical streams, reports and
checkpoint-codec bytes. This fixture disables final materialization and asserts
that its concrete checkpoint has no entries; nonempty codec round-trip coverage
is separate. Its scoped readiness hooks are test-only, and the fixture makes no
host timing claim.

Replay checks the source fields allowed by each origin kind, requires
an archive checkpoint exactly when the stream names one, and matches the supplied
checkpoint path, format, serialized bytes, and digest before copying its provenance
into a completed report. Archive-origin recording and full or prefix replay require
an `ArchiveSource` built by parsing and hashing one byte stream. Its private report
and digest cannot be replaced separately, and replay checks that raw-byte digest
against the recorded origin before deriving resume input or importing evidence.
Changing only JSON whitespace still changes the source identity. Live archive
import uses that same sealed source. Each exported archive snapshot also has a
source-entry SHA-256 digest of the workload's logical snapshot identity. The
workload chooses the projection; Consonance-backed workloads exclude only
restoration metadata that their existing job-result identity also excludes.
Before an archive checkpoint restores any state, live import and replay check
each checkpoint entry's ID and snapshot against that source-bound digest.
Duplicate or unknown checkpoint IDs, and source entries without a bound digest,
are rejected. Whole-tree import also compares every present source-entry digest,
including genesis, with its newly reconstructed logical snapshot identity before
admitting that entry or inheriting source evidence. A source entry that had a
digest but no longer reconstructs to a runnable endpoint fails import. Older
archive reports without snapshot identities remain usable for whole-tree import
without a checkpoint, but cannot authorize checkpoint restore or certify a
source endpoint across imports. Their actions are reconstructed under the current
executor; the source progress watermark remains inherited historical evidence
bound to the source file bytes, not evidence produced by that reconstruction.
Snapshot-root origins use their separate checkpoint contract.
For a workload with endpoint certificates, whole-tree import reconstructs each
action between a parent and its imported candidate, including candidates loaded
from source snapshots. It compares a checkpointed endpoint with the rebuilt
endpoint before binding any certificates to the archive path. A later snapshot
eviction can therefore replay the imported path with the same checks as a live
campaign. `TargetExecution::apply_action_and_snapshot` can return the actual
runnable snapshot captured by that action. Import uses its certificate for that
prefix and retains the final capture as the rebuilt endpoint after checking the
execution disposition. A workload returning no capture uses separate certificate
and snapshot operations. Empty suffixes acquire a fresh snapshot; terminal or
failed actions cannot reuse an earlier capture. These values remain local to the
import operation and preserve all source identity and intermediate-prefix checks.
It also checks the fixed executor and selection-seed derivation labels. The stream and
completed report record the loaded Harmony executable identity separately from the
workload identity. Linux hashes the loaded image through `/proc/self/exe` once per
process, because the loaded image cannot change while it runs; macOS reads
its running code-directory hash. A wasm32 module cannot read its own bytes, so it
records the fixed label `wasm32-module-v1`, which does not bind replay to one build.
Replay fails closed when that identity cannot be
read or differs, even if the guest image and workload policies match. The campaign
reads the identity once before writing the stream and uses the same value for its
checkpoints. Replay reconstructs adaptive mixture weights from each admitted
result at the original reservation boundary and rejects job or skip weights
that the recorded policy could not have produced. When the recorded policy
stops the campaign on an objective, replay rejects work selected after it while allowing jobs
already reserved in the admission window to finish. The same admission-window
rule applies when cumulative work reaches the recorded work budget. Schema version 14
requires the loaded executable identity and records the reservation that owns each
duplicate skip. Replay checks that skips follow
selection order, stay within the admission window, and do not exceed the live
limit of 1,024 attempts for one reservation. Each skip must occur during the
initial window fill or immediately after the admission that opens its later
reservation slot. A reservation that takes a continuation cannot record a
duplicate skip because continuation dispatch bypasses ordinary selection. A
terminal record follows only
after the campaign has drained its jobs and finished
successfully. Replay requires that record and checks its job, skip, and
admission totals. Without a wall-time limit, it also requires every reservation
the deterministic stop policy would have made; intermediate prefix replay is
still allowed. Thus a stream cut after a complete
JSON line cannot be certified as a finished campaign. Callers that deliberately
rebuild an intermediate state use `replay_campaign_prefix_checkpointed`, whose
report mode is `campaign-prefix` and is not a completed-campaign certificate.
Reserved jobs pin the
snapshot they actually restore, including a
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
same archive state without repeating selection. The recorded parent choice is
an input to replay, while its tier rank must be below the archive population
limit because no selector draw can name a tier beyond that many active entries.
The final report selects no
more parents, so its compaction releases metadata pins and the liveness anchor
and then requires the compacted archive to fit the memory limit. Campaign streams require
schedule policy version 6
and the current bounded progress policy; recordings from superseded policy
namespaces are rejected before replay because their memory accounting differs.

Empirical step tables fold retained suffixes into an incremental hash and a
deterministic frequency map capped at 4,096 distinct steps. A draw picks a step
with probability proportional to its count over all retained suffixes, with no
recency weighting. The compact table is the only supported representation. Every campaign keeps one, in
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
targets than workers. The first worker's boot time includes that wait. Every
worker reports its startup result before the coordinator selects or dispatches
a job. If several workers fail to start, the lowest worker number determines
the reported error regardless of host timing.

Workers pull jobs from one shared queue ordered by planned finish, so an idle
worker takes the queued job that admission reaches first while another worker
is still busy. A short job reserved after longer ones runs before them,
because admission needs its result first. The coordinator dispatches each job
when it reserves it, so the admission window alone limits the jobs that are
queued, running or finished but not yet admitted. Memory held by finished
results is outside the archive's logical budget and must be measured in host
RSS. Telemetry charges idle worker time to the pool: `idle_admission_order_ns`
when finished results wait on an earlier job and `idle_no_job_ns` otherwise.
Worker execution errors also wait for their reservation's planned admission;
the coordinator can record every earlier successful job and its checkpoint
before returning a failure identified by reservation. Checkpointed coordinator
profile timings are observational telemetry, so checkpoint hashes may differ
across host schedules even when their search state and resumptions agree.
An execution panic produces a stable error reply carrying the dequeued job's
schedule key. The worker discards its target and builds a fresh one before
taking another job, so a queued earlier reservation can still complete. If
recreation fails, the worker retires. Other workers may continue queued jobs;
if all workers exit with outstanding reservations, the coordinator drains
received results and reports the first missing planned reservation. Target
recreation failure is an external failure, so its timing can change which
outstanding jobs complete before the failure is reported.
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

The archive charges its own heap from logical counts: entries, stored suffix
actions, live prefix nodes, slots, cells, active entries, tiers, pins, landing
markers, continuation edges and shared snapshot allocations. A checkpoint
restores those counts, so a resumed or replayed archive charges what the live
one did. Each count is multiplied by a worst-case size under glibc malloc. An
allocation of n bytes takes n plus an 8-byte header, rounded up to 16 bytes, and
at least 32 bytes. A vector grown by push holds at most twice its length and at
least 4 elements, or 8 for bytes. A B-tree node holds 11 elements, and every
node but the root holds at least 5, so a set of maps holding n elements uses at
most n/5 nodes plus one per map. Each prefix node is charged one child-map node.
The selector's weighted sets keep freed slots until the selector index is
rebuilt, and compaction rebuilds it, so freed slots are charged as one per
inactive entry. Compaction shrinks the entry columns to their length so the
doubling bound holds afterwards. Each resident snapshot also charges its
reference-count block. On synthetic archives these charges sit 15-50% above the
measured heap; the `memory_tests` unit tests count the archive's allocations and
fail when any part exceeds its charge. Allocator fragmentation and heap a
workload's snapshot charge omits stay outside the archive's limit and show only
in RSS.

Prefix-tree compaction remaps a single surviving child in its existing map.
The same prefix nodes can hold optional workload endpoint certificates. A
retained candidate binds the certificate for each action since its parent,
including intermediate actions that were not themselves retained. A job
origin returns the replay actions and certificates from the same node walk,
and in-flight checkpoints preserve both vectors. Resume rebuilds each job's
vectors from the snapshot the job pinned at dispatch and rejects a checkpoint
whose in-flight jobs do not match the archive's snapshot pins. Shared prefixes
reject a conflicting binding; compaction and snapshot eviction keep nodes on any
selectable entry's path. Workloads without endpoint certificates leave these
certificate slots absent, while workloads that require them reject missing values during
replay.
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

An `ArchiveKey` names four things about a state, and nothing else reads a
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

The archive memory budget covers retained snapshots and job-origin snapshots
pinned until their ordered results are admitted. A workload reports exclusive
snapshot bytes separately from a root-to-parent visitor over shared allocations.
The visitor identifies actual live allocations, including their reference-count
headers. The archive charges each reachable allocation once as a malloc block
and charges the ownership ledger as one B-tree map. Acquiring or releasing a
root follows only newly owned or finally released nodes, stopping at an already
owned ancestor. Clones of an existing root cost one lookup; budget queries do
not traverse ancestry. Allocation identities and the ownership ledger are
runtime-only and never enter deterministic evidence.

Worker target-private state, including independently retained inner history
handles after a reply, remains outside this archive budget. Job-origin outer
snapshot handles are dropped before the reply releases their ordered pins.
Thus legal worker drop timing before unpin cannot change budget decisions.
The archive owns a typed workload retention adapter alongside the checked
allocation ledger. After a candidate wins retention, the adapter normalizes its
validated expected history against currently retained nodes, walking only the
new tail to an indexed ancestor. The adapter registers nodes when the ledger
first acquires them and removes registrations when the final root or parent
edge releases them; its index storage is charged as well. The incoming root is
acquired before population retirement or replacement releases old roots, so
replacing the sole owner of a deep prefix still walks only the new tail. Candidates
rejected or failing before installation cannot leave registrations. Input-prefix
ownership and
certificate bindings are committed only after normalization and population
admission succeed. Retirement clears owners without pruning prefix nodes, so
the validated prefix start remains available for that commit. Independently allocated worker
histories therefore enter the archive with the same physical sharing as restored
histories.
Checkpoint restoration feeds roots through the same normalization and acquisition
path in stable entry order. Imported snapshots use the already admitted bootstrap
root in that index. Sharing changes actual allocations rather than treating equal
content digests as shared storage. The index has no global lifetime and independent
execution receipts retain their own validation authority.

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

One selector exists, `tier_progress_cascade_pace_yield_cell_recent_count_decay_v2`, and the stream header names
it as `parent_scheduler`. A draw walks three levels. The tiers are the distinct
progress values held by selectable entries, ranked from the deepest; a tier at
rank `r` weighs `1 << ((8 - min(r, 8)) * 3)`, an eighth of the rank ahead, so
the leading tier takes most of the draws, and no tier holding an entry takes
zero. Every workload uses this ratio, so a workload's progress value must not
fall along the route to its goal: states on that route would rank below a tier
that leads nowhere and receive almost no draws. The
replay verifier bounds a recorded rank by completed progress levels in
previously admitted archive entries when that selection was reserved. The
bound includes historical levels that may no longer be selectable, so it
rejects impossible ranks without claiming to
reconstruct every selector draw.
Replay also rejects a best-holder preference index outside the workload key's
compiled portfolio; otherwise selector accounting would silently ignore it.

The leading tier gives up weight while it makes no progress. A tier makes
progress when a parent drawn from it opens a cell at a deeper progress value;
a new cell beside its parent at the same progress value is not progress. Each
tier counts its draws since it last made progress, or since it appeared, and
keeps the longest such run; its pace is that longest run or its cell count,
whichever is larger. When the current count passes twice the pace, the leading
tier's weight halves, and it halves again each time the count doubles, down to
the weight of the rank below. The halvings apply only while the rank below
yields more per draw than the leading tier since the run began, or while the
leading tier has gone longer without a yield than the rank below has been
drawn in the run, or while it has gone more than eight times its own average
gap without a yield. A tier's
yields are its new cells and its carried-in wins, admissions that take a
slot's preference from a parent in another cell. A cell is new when the
archive has never held it and no higher tier holds a cell at its place, so a
cell that comes back after its entries were dropped, or a lower tier reaching
a place a higher tier already holds, yields nothing. The rank below counts
only its new cells in the comparison, so a lower tier that keeps refilling
stock at the same places does not take weight from a leading tier that keeps
opening ground. With `Td` and `Ty` the leading tier's draws and yields in the
run, `Tq` its draws since its last yield, and `Nd` and `Ny` the draws and new
cells of the rank below over the same span, the halvings apply while
`(Ny + 1) * (Td + 1) > (Ty + 1) * (Nd + 1)`, `Tq > Nd`, or
`Tq * (Ty + 1) > 8 * Td`. The second condition
makes a leading tier that stops yielding lose its weight even when the rank
below yields nothing: `Tq` grows with every leading-tier draw and `Nd` grows
only with the rank below's smaller share, and a tier's yields are finite. The
third condition sets how soon the fall starts: a tier that still wins now and
then, but far less often than it used to, falls without waiting for `Tq` to
pass every draw the rank below received since the run began, while a tier that
keeps farming at its usual gap holds.
When the rank below is a different tier from the one recorded when the run
began, or no tier was recorded, `Nd` and `Ny` are that tier's totals. A
leading tier that yields at least as much per draw as the rank below opens
cells keeps its full weight. Progress from the tier restores its full weight.
A dead-end route that keeps opening cells beside each other at its deepest
progress value therefore still loses weight, because its cells are finite and
each opens no deeper ground. When the leading tier has halved all the way
to the weight of the rank below, the same rule is applied to that rank against
the rank below it, and so on down. Every rank down to the first one still short
of its full halvings shares that rank's halved weight. This lets the search
fall back past several stalled tiers at once, which matters when the states
that decide a later death lie several tiers back. Within the tier each cell
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
run, its total draws and yields, its yields in the current run as `wins`,
its draws since its last yield as `dry`, its new cells as `cells`, and the rank below's name, draws and new cells when
the run began, and the draws each
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
Campaign replay rebuilds ordinary splice selection before each recorded job or
skip and rejects a donor, leaf, or action tail that differs from that selection.

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
Replay rebuilds the parent's length and the limits held by earlier in-flight
reservations before accepting each recorded job limit. It cuts the redrawn
suffix to the recorded limit. It rejects a drawn job whose limit is missing or
outside 1 to 64, and a spliced or continued job that records a limit. Replay also checks each drawn skip limit against the in-flight reservations at
its selection point, including skips before the first admission. Reservations
prefilled later in the window are excluded from that earlier selection state.

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

Optional job observations use `run_campaign_checkpointed_observed`. Each workload
owns its `CampaignTypes::JobObservation` schema; uninterested workloads use `()`.
The payload is `Copy + Send + 'static` and compile-time limited to 256 bytes. It
must contain self-contained values: `Copy` excludes owning containers, but cannot
prove that a static reference or integer handle does not refer to external data.
Searcher never decodes the workload payload and does not serialize it into the
logical stream, report or checkpoint. Collection does not call cumulative target
telemetry; final worker telemetry still runs once per worker.

The optional sink receives validated completion receipts with the actual physical
worker, reservation, planned finish and reserved parent identifiers. Completion
order is physical; consumers join by reservation rather than assigning logical
meaning to delivery order. The sink runs synchronously and can affect host timing.
A sink error or panic disables subsequent sink calls, preserves the primary
campaign result and records the coverage loss in caller-owned
`ObservationDiagnostics`. Counters survive campaign errors and unwind. A reply
owns a small delivery envelope; dropping an unsent, invalid, queued or undelivered
payload counts one discard without allocating or locking during Drop. After the
campaign and worker scope fully terminate, collected equals delivered plus
discarded, unless the explicit overflow flag is set. Hook panic counters count
events: one job can increment both begin and drain panic counts, so those counts
are not a disjoint partition of eligible jobs. A snapshot while producers are
active is observational and is not a final coverage receipt. Consumers must
bound their own output rows; no internal per-job log is retained.

`telemetry.completed_buffer` reports live and peak `results`, `actions` and
`candidates` counts for the current coordinator invocation; checkpoint resume
starts fresh gauges and peaks. A result enters when the coordinator receives it
and leaves when ordered admission removes it. Failed worker results count as one result
with zero action/candidate records; candidates count every present optional
candidate, including nonviable ones. Successful reports end with zero live
counts; peaks include immediately admitted results as well as held results.
Coordinator profile lines expose the same action/candidate live counts and all
three peaks beside the existing `completed_buffered` result count.
These are record counts, not heap bytes, unique snapshots or process RSS. They
are runtime diagnostics excluded from checkpoint profiles, search decisions,
report equality and the deterministic report/stream serialization.

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

Scheduling stays on the coordinator. At a due admission boundary it joins the
preceding publication, captures the header/body and freezes owned workload
snapshots, then hands the image and exclusive store writer to one thread.
Body, evidence and draw serialization remain synchronous capture work; snapshot
DAG encoding and durable filesystem work run on that thread. An image never
reads the live archive, counters, random state, pending jobs or draw tables.
The final publication is joined before `End`, the stream digest or a completed
report. The direct coordinator owns the same publisher as the native pool wrapper.
Its existing fallible reserve, admission, failure and finish operations hand back
persistence errors; `plan` and admission watching keep their public signatures.
An admission-gap refusal also joins the last image before returning. Errors from
these state-mutating operations or the native pool wrapper join the writer and
retain any primary execution error alongside a typed persistence failure.
Unwinding also joins without detaching.
A failed publication permanently retires its writer and latches the persistence
error, including failures after checkpoint rename or journal writing. Charge,
freeze and synchronous body-capture errors retain their original type on first
handback and remain the typed primary/source during finalization; the latched
summary only rejects later operations.
Evidence writers retain their original boxed errors from both the streamed
byte-count and byte-emission passes, including the actual coordinator capture
path. Successful streaming preserves the existing vector codec bytes, and a
serializer or sink failure without a writer error keeps its Postcard error.
Once an error is handed back, the direct coordinator refuses further reservation,
admission and completion; `plan` yields no additional jobs. Workload integrity
is checked before persistence polling, so an already latched typed contradiction
remains primary whether the writer is ready or still pending. Finalization keeps
its typed persistence secondary and never emits `End` after either failure.

`CheckpointPlan::max_image_bytes` bounds one immutable image and its private
allocation ledger. Package entry points use 64 MiB. This is a staging resource
bound, separate from archive retention and from RSS: existing codec working
memory, writer DAG indexes and the fixed output buffer remain separate costs.
`CheckpointSnapshot` implementations explicitly charge frozen owned capacities,
container overhead and immutable shared allocations before cloning. Shared
allocations use domain-separated host-local keys and are charged once per
image; these keys never enter checkpoint bytes. Sparse portable snapshots own
their page/chunk references, and a workload needing other external resources
must acquire immutable leases in its freeze implementation. Freezing grants no
execution certificate or page-verification exemption. Default browser coordinators
have no file publisher; explicit file-checkpoint plans on wasm32 are refused at
publisher creation before any file or thread operation.
Container charges reserve twelve key/value slots plus 256 bytes for each owned
B-tree entry, including the private allocation ledger. This deliberately covers
an entire node for even a one-entry root under the pinned Rust toolchain rather
than assuming a densely occupied tree. Vector/string charges use owning
capacities; allocator fragmentation is outside this structural byte budget.

The image uses a separate outer allocation; it never clones the archive's outer
snapshot `Arc`, whose strong count affects reclamation. A coordinator keeper
holds the image until the next scheduled checkpoint boundary even when the
writer finishes early. Completion timing cannot change snapshot residency or
parent selection. There is no queue, coalescing or completion-driven checkpoint
cadence. If a snapshot charge or bounded body capture exceeds the image limit,
the coordinator performs that boundary synchronously through the same codec
and publication engine before freezing an oversized image. Evidence, draw,
JSON-backed fields and in-flight records stream from borrowed boundary data on
this path, so pressure does not first allocate an oversized intermediate Vec.
Rejected header and policy buffers are also borrowed on the pressure path. The
synchronous writer serializes a private borrowed view with the publication owner
override, preserving the owned header's field order and bytes. An owned image
header is cloned only after its complete allocation charge has been accepted.
The capture sink retains at most its accepted capacity; after confirmed image
overflow it discards further bytes while reporting successful writes. Capture
still completes synchronously, so an independent body or workload writer error
remains primary and retires publication without retrying the callback. Only a
successful overflowing capture falls back to synchronous publication. This can
add a complete synchronous body traversal before that fallback, rather than
stopping at the first over-limit write. It adds no oversized body allocation or
snapshot freeze, and does not change successful codec bytes or checkpoint cadence.

Snapshots go into one append-only `snapshots.store` per directory. An archive
entry's snapshot never changes, so each is written once and later checkpoints
list it by entry id, offset, length, and SHA-256. Resume checks each referenced
snapshot's bytes before decoding it. The writer counts the kept checkpoints that
list each stored snapshot. Deleting an interval checkpoint decrements the
counts of the snapshots it listed. A snapshot no kept checkpoint lists leaves
the writer's map, and on Linux its byte range in the store becomes a hole
(`fallocate` with `FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE`). The file
keeps its length and every other offset stays valid, so no checkpoint file
changes. The writer syncs the directory after deleting a pruned checkpoint and
before punching, so a crash cannot bring back a checkpoint whose snapshots are
gone. On macOS, and on a Linux filesystem without hole punching, the freed
bytes stay in the file. Each `.ckpt` file holds its header, that index, the
postcard body, and a trailing SHA-256 of those preceding bytes. Resume reads
the format name first, then checks the checksum before decoding the rest of the
header or body, so byte changes that remain valid postcard data fail closed
instead of changing the continuation. These
checks detect corruption, not deliberate rewriting of a checkpoint and its
checksum. `checkpoints.jsonl` records each checkpoint's SHA-256, write time and sizes:
`store_bytes` is the store's length. `live_snapshot_bytes` includes endpoint and
shared frames reachable from kept checkpoints; `new_snapshot_bytes` and
`freed_snapshot_bytes` include both kinds. `new_snapshots` and `freed_snapshots`
count endpoint frames only. The writer syncs dependency frames before publishing
references, syncs each completed checkpoint, renames it and syncs its directory
on every publication, including milestone, tier and final checkpoints. It then
syncs the appended journal record and its directory before acknowledging.
Checkpoint files and their referenced frames are authoritative for resume; the
synced journal binds their checksums and records diagnostics, but is not a
replacement for byte validation. New checkpoint directories and their
containing entries are synced when created. Pruning still syncs deletions before
releasing frame ownership or punching. An error after a rename can leave a
complete additional checkpoint, but cannot acknowledge success or damage an
earlier kept checkpoint.

The
header names the body's layout, `SEARCH_CHECKPOINT_FORMAT`. The name changes
whenever a stored type such as `Archive` changes, and a reader refuses a
checkpoint of any other layout before it decodes the body. The
archive stores its `SelectorAccounting` as JSON inside that body, so a counter
added there, such as `tier_runs`, reads as empty from a checkpoint written
before the counter existed.
The header also carries the optional search owner supplied by `CheckpointPlan`.
Harmony's saved-run manifest records that owner before search begins and checks
it before resume, so a checkpoint from another run cannot impersonate the
interrupted run's latest checkpoint. A completed saved run also binds the
selected checkpoint's SHA-256. `CampaignOrigin::SearchCheckpoint` resumes one.
When that origin carries a selected SHA-256, replay compares it with the exact
checkpoint bytes opened for resume before restoring state.
The admission window, limits, workload identity, loaded executable identity,
and the workload policies that give stored inputs and keys their meaning must
match. The search checkpoint format is version 9; a checkpoint from another
executable is refused before the new stream is written. Within that same
executable, the generic campaign configuration may change mixture, retention
and either objective-stop setting at runtime. The selector and continuation
identifiers are compiled constants; draw-table and preference identifiers come
from workload behavior. The resume policy map structurally permits differences
in these identifiers, but any change requiring a rebuilt executable fails the
executable-identity check. A searcher code change likewise requires
comparison from rooted inputs or a fresh power-on run. Accepted runtime
policy changes appear in the origin record and stream header as
`checkpoint_policy_changes`. The same seed
repeats the original progress lines; another seed derives a new selection random
state and keeps everything else. The draw tables continue their table hash
from the recorded one, so stream draw-table hashes after a resume differ from an
uninterrupted run. A resumed stream cannot be replayed. The checkpoint stores
every reservation between the next admission and the last reservation, whether
queued, running or finished, and the resumed run dispatches them again, so it
may use a different worker count. If the checkpoint was taken while a budget
was draining the reservation queue, a larger resumed budget fills the vacant
slots up to the recorded window before admitting another result. Workloads opt
in through `Reporting::evidence_checkpoint` and
`Reporting::evidence_from_checkpoint`.


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

TargetExecution::finish_coordinator_session ends coordinator-owned bootstrap or
checkpoint-resume resources before worker readiness is released. The default is
empty for workloads without such resources. The scope is installed before target
construction; normal success and errors invoke it once, while unwinding invokes
best-effort cleanup. A cleanup error fails successful bootstrap. If execution
already failed, its original typed error is retained and cleanup failure is
reported separately. Worker targets and their in-campaign reuse are unaffected.

The same coordinator-session scope surrounds the entire standalone full or prefix
campaign replay, including validation, target construction, replay errors and
unwinding. A replay reuses its session within that operation and finalizes it
before returning. During a live campaign the coordinator signals bootstrap
readiness only after finalization; other execution workers may initialize
concurrently, and accepting whole-pool readiness still waits for that worker.

## Stepwise coordination

`CampaignCoordinator` holds a live campaign: the `CoordinatorCore`, the seeded
selection state, the draw tables, the duration policies and the admission
window. A single-threaded caller drives it with these calls:

| Call | Effect |
| --- | --- |
| `CampaignCoordinator::start` | Validates the config, writes the stream header and bootstraps or restores the archive inside the coordinator session. |
| `reserve_next()` | Reserves one job of the first window and returns false once the window is full or selection stops. The worker pool calls it after every worker reports its startup result and dispatches each job as it is reserved. |
| `plan(limit)` | Hands out up to `limit` reserved jobs, lowest planned finish first. |
| `admit(outcome)` | Takes one finished job, admits every job that admission order now allows and reserves one job after each admission. |
| `fail(reservation, error)` | Records a job that failed to execute. The campaign returns that error when admission order reaches the reservation. |
| `finished()` | Reports that every reservation is admitted. |
| `finish(workers)` | Writes the final search checkpoint and builds the report. |

`archive()`, `evidence()`, `executions()`, `reserved()`, `objective_witness()`
and `progress()` read the live state for display. `progress()` returns the
same `CampaignProgressRecord` the progress sink receives.

A worker runs a `JobSpec` with `run_job` and returns a `JobOutcome`. A
`JobSpec` carries its reservation, planned finish, parent snapshot, replay
actions, their endpoint certificates and suffix. `run_job` executes it through
`execute_job_certified`. A `JobOutcome` carries the `CampaignJobResult`, the
execution work and the `certified_result_sha256` digest. Both derive serde, so a caller can pass
them between threads or browser workers as postcard bytes. A workload's
`Observations` must therefore deserialize.

`run_campaign_checkpointed_with_options` and
`run_campaign_checkpointed_observed` drive the same coordinator from the
worker pool. Each plans every reserved job into the pool's queue and admits each
reply. A reservation follows an admission and never a caller's request, so a
caller that plans any number of jobs and runs them in any order writes the same
stream as the pool for the same seed and window.

`run_campaign_watched` runs the same pool with a `CampaignWatch`. Its `run`
hook wraps each job on the worker thread and returns a note with the outcome.
Its `admitted` hook receives that note and the coordinator on the coordinator
thread after each admission, and `start` runs once after the coordinator
starts; an error from `start` stops the campaign. `watch_admissions` makes the
coordinator keep each admission's decisions, and `take_admissions` drains them,
so a watcher reads retained ids without parsing the stream.

The crate builds for `wasm32-unknown-unknown`. On wasm32 the telemetry clock
reads zero and progress lines carry `unix_time` 0, because `std::time::Instant`
and `SystemTime` panic there. A wall budget never stops a wasm32 campaign.
Search checkpoints and objective witness files need a filesystem. The worker
pool and thread placement need threads.

```sh
cargo check --locked --manifest-path dissonance/Cargo.toml -p searcher --target wasm32-unknown-unknown
```

## Browser adapters

`Archive::entry_input` materializes an entry’s complete ancestral action tape
for inspection or playback and rejects missing indices. A browser adapter can own
execution snapshots outside the archive and use this read accessor for films.
Weighted draws use `RomuDuoJrRand::below_u64` for 64-bit weight
totals, so wasm32 does not truncate them through `usize`. This uses the same
multiply-high draw as native selection and preserves its random sequence.

Snapshot artifacts select a workload-owned `CheckpointSnapshot::Codec`. The common
`HMSNAP02` envelope streams checksummed shared-record and endpoint frames, validates
parent-before-child dependencies and the exact referenced closure, and refuses old
flat envelopes and trailing bytes. Plain snapshots opt into the postcard codec;
workloads with persistent graphs provide semantic keys and graph validation. All
artifact writers, readers and source-bound checksum validation use this codec.

`CheckpointSnapshot` also supplies explicit immutable freeze and allocation-charge
contracts. `Reporting::write_evidence_checkpoint` and borrowed draw/JSON writers
preserve existing body bytes without first materializing unbounded temporary
vectors. These contracts prepare bounded publication images; frozen state does
not grant execution or import trust.

Whole-search checkpoint v10 uses the same workload codec in a typed `HMSTORE2`
append store. Snapshot frames refer to semantic shared keys using checksummed
physical ranges. Shared frames carry earlier parent locators, so resume validates
the reachable graph iteratively without a global dictionary in each checkpoint.
Range overflow, overlap, type/key conflicts, forward references and incompatible
codec headers fail before archive installation.

The writer stages new metadata until the checkpoint is synced and renamed. Kept
checkpoint listings retain endpoint frames; each live frame owns its root or parent
edges once. Removing the last listing cascades through unowned graph records
iteratively, forgetting their keys before punching file holes. Later reintroduction
of the same semantic root emits its missing ancestry with new locations. Pinned
checkpoints preserve the reachable graph. Failed encoding stops that writer;
failed checkpoint publication cannot change earlier live listing/edge ownership.
Appended unreferenced tails may remain after a fatal failure. The bounded immutable
image and publication worker described above own persistence; synchronous pressure
handback uses this same codec and publication order.

Whole-search checkpoint v10 combines the current coordinator/archive body with
shared snapshot frames. Both earlier v9 layouts are refused before decoding a
body: upstream's flat snapshot store and the earlier shared-frame layout used
by the integration branch. There is no legacy format fallback. The shared
codec remains workload-owned; assurance metadata is interpreted and validated
by that workload's codec, never by generic storage or routing.
