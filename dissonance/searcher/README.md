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
for replay.

## Workload adapters

- `smb/` maps QuickNES WRAM to Super Mario Bros. observations, room/depth keys,
  milestones, and controller-chord policies.
- `nova/` maps Nova system/save RAM to spatial keys, level and collectible
  progress, milestones, and its input vocabulary. Its optional `consonance`
  module drives the Consonance control protocol for a live guest.
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
