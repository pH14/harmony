<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# searcher

`searcher` implements deterministic search independently of a workload. The
`search::` modules own archive retention, parent selection, input mutation,
campaign coordination, worker execution, seeded draws, checkpoints, stream
recording, and replay. The `Game` trait supplies target construction, action and
snapshot types, archive keys, observations, progress, and workload policy
identifiers.

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

## Design goals

Games are evaluation workloads for improving general search. Success on one
particular game is evidence about a mechanism, not the objective that defines
that mechanism. Keep routes, obstacle-specific strategies, weapon choices, and
location-conditioned input weights out of the searcher and its evaluation
adapters. A game-neutral API alone does not prevent overfitting: key grouping,
progress ordering, state preference, and input policy also shape the search.

Adapters supply the smallest justified mechanical observations and capabilities.
Separate location identity from progress and same-location preference; a larger
room number or coordinate need not mean advancement. Put diagnostic detail in
reports before making it search-visible. Version semantic policy changes and
compare engine changes under fixed adapter policies across multiple workloads.
An unsolved, correctly instrumented game is a useful evaluation result.

The following performance properties are design goals, not rigid throughput
promises or claims that the current implementation has achieved them:

- **Scale search with compute.** While independent useful work remains, more
  cores should continue buying more explored alternatives, including on large
  machines. Avoid a fixed coordinator, lock, queue, or archive-maintenance
  bottleneck that makes additional workers useless. Measure useful breadth
  and progress as well as raw executions; redundant work alone is not scaling.
  Long individual jobs and stale admissions can waste an otherwise full pool.
- **Degrade gracefully under memory pressure.** An operator should be able to
  trade retained diversity and extra reconstruction work for a smaller memory
  budget while the search continues admitting and exploring new states. Keep
  an executable origin and reproducible retained witnesses. Below the minimum
  viable state plus required working space, a clear error is appropriate.
  More memory should generally improve search opportunity; individual seeded
  runs need not improve monotonically as eviction changes their trajectories.
- **Spend compute on exploration.** Keep allocation, copying, hashing, snapshot
  restore, bookkeeping, and observation costs proportionate to useful work.
  Prefer bounded/incremental maintenance and shared or compact state where
  measured costs justify them. Avoid repeated full-history scans, rebuilding
  full input prefixes, or allocating full memory images in the per-job path.
  Profile the engine and backend separately before optimizing either.
- **Sustain hours of search.** Archive growth and culling should preserve useful
  alternatives and continued admission without increasing cost with all past
  executions. Bound queues, cached routes, dead metadata, histories, and
  diagnostics as well as snapshots. Checkpoint, reporting, and shutdown costs
  are part of campaign performance: a healthy search followed by a huge memory
  or disk spike is an incomplete result. Preserve compact replayable evidence
  without requiring every historical full input to be materialized at exit.

### Current mechanisms and limits

The coordinator is still serial for selection/admission, with a deterministic
sliding window feeding physical workers. Worker count and window policy are
part of the search configuration; determinism does not require equal campaigns
at different worker counts. Admission order, rather than host completion time,
must continue determining replayable decisions. Wall-time telemetry may measure
performance but must not enter those decisions.

Memory budgets currently charge logical live structures conservatively. Cached
snapshots are released before entries, nearby keyframes bound reconstruction,
an executable liveness anchor survives eviction, and metadata is compacted in
batches. Reserved snapshots remain charged until their jobs are admitted.
Incremental maintenance can temporarily exceed the logical target. The budget
is not an operating-system RSS cap: worker/backend state, in-flight allocations,
allocator overhead, and output materialization must also be measured. A library
entry must account for its own snapshot and policy state honestly.

The compiled entry ceiling still rejects admissions when reached; it is a
safeguard, not satisfactory long-horizon culling. Raising it alone does not meet
the memory or sustained-search goals. Full final archive/checkpoint output can
also be substantially more expensive than steady-state search. These limits
must remain visible in measurements rather than being described as solved.

### Evidence for performance changes

Hold ROM/core, origin, observation and input policies, logical limits, and seed
set fixed for before/after comparisons. Report outcomes and cost separately:

| Question | Measurements |
|---|---|
| Does extra compute buy breadth? | Worker sweep (1, 2, 4, then available larger counts), novel states/milestones at equal elapsed budgets, frames/s, worker utilization, coordinator time and job-duration tails |
| Does less memory remain useful? | Several feasible budgets including a constrained case, progress/admission after eviction, logical charge, peak RSS, replay work, evictions/compactions and preserved witnesses |
| Is execution efficient? | Time in emulation, restore, decoding, selection/admission, hashing and allocation; bytes allocated/copied per job where profiling supports it |
| Does the campaign age well? | Early versus mature windows at comparable work, active/historical entries, queue/cache sizes, memory trend, stream/output growth, checkpoint/export time and peak space |

Report executions, emulated frames, and wall time to a declared milestone.
Execution counts alone are not comparable when suffix or replay cost changes.
Keep startup, active search, and final export timings distinct. Do not turn
non-completion at a cap into an estimated completion time; report the budget
and achieved progress, with median/worst outcomes across registered seeds.
Use small correctness/pilot runs before expensive sweeps or long soaks. Changes
to retention, scheduling, or accounting also need deterministic replay checks
under pressure and workload-neutral tests of the changed invariant.

## Workload adapters

- `smb/` maps QuickNES WRAM to Super Mario Bros. observations, room/depth keys,
  milestones, and controller-chord policies.
- `nova/` maps Nova system/save RAM to spatial keys, level and collectible
  progress, milestones, and its input vocabulary. Its optional `consonance`
  module drives the consonance control protocol for a live guest.
- `stb/` maps the source-built Super Tilt Bro local-AI match to paired fighter
  observations, durable knockout progress, and a QuickNES controller
  vocabulary. Its `stb-probe` and `stb-campaign` binaries record setup,
  replay, and bounded campaign evidence without committing the ROM.
- `target.rs` provides the smaller action/observation/snapshot seam used by
  target implementations and tests.

The campaign binaries under `src/bin/` write a report, recorded stream, and
checkpoint. Replay consumes those artifacts and verifies the recorded decisions
and observations against the same workload identity.

Run the library checks with:

```sh
cargo test --manifest-path dissonance/searcher/Cargo.toml
cargo clippy --manifest-path dissonance/searcher/Cargo.toml --all-targets -- -D warnings
```
