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

The legacy selector identifiers retain their exact behavior. Two opt-in search
experiments are versioned independently:

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
