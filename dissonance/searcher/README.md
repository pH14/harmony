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
resulting ordering and retains bounded representatives. It also reads the key's
own `Ord` as depth — the deepest live key a run reports, the donor order inside
a selection cell, and the splice gate all compare keys directly — so a field the
grouping never reads still ranks depth if the key sorts it early.
`group_depth_cmp` is the ordering a declared grouping implies, and
`first_depth_ord_disagreement` reports where a key's own ordering departs from
it. Whether a departure helps or hurts is a search question to measure, not a
defect to assume: Nova sorts resources first on measured evidence, while SMB's
ordering departs from its grouping in the opposite direction. Campaigns reserve jobs
in a deterministic admission window, allow physical workers to execute them,
and process results in recorded admission order. The stream records the
configuration, policies, origins, jobs, admissions, skips, and progress needed
for replay. Reserved jobs pin the snapshot they actually restore, including a
parent's keyframe. If retention removes that snapshot from the active population,
its memory stays charged until the last reservation is admitted. Live execution
and serial replay release these pins at the same recorded boundary, independent
of worker completion timing. Budgeted streams before schedule version 3 are
rejected because they used different snapshot accounting.

## Workload adapters

- `smb/` maps QuickNES WRAM to Super Mario Bros. observations, room/depth keys,
  milestones, and controller-chord policies.
- `nova/` maps Nova system/save RAM to spatial keys, level and collectible
  progress, milestones, and its input vocabulary. Its optional `consonance`
  module drives the consonance control protocol for a live guest.
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
