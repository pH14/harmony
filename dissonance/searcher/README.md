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
