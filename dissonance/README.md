<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# dissonance

dissonance is a standalone workspace for deterministic search over a machine
boundary. It is separate from the consonance workspace so target execution,
search policy, and hardware-backed control remain independently testable.

## Workspace crates

- `machine/` defines the snapshot, branch, replay, run, and read interface and
  provides the native QuickNES adapter used by the NES workloads.
- `searcher/` contains the game-neutral archive, campaign coordinator,
  selection, mutation, worker scheduling, recording, and replay code. Its SMB
  and Nova modules supply target-specific observations and policies.
- `fuzzer/` contains the current auxiliary fuzzing entry point and is kept
  separate from the library crates.

The generic search layer sees actions, observations, snapshots, ordered archive
keys, and opaque policy values. Game addresses, setup sequences, progress
interpretation, and state preferences belong to the workload adapters.

## Running checks

```sh
cargo test --manifest-path dissonance/Cargo.toml
cargo clippy --manifest-path dissonance/Cargo.toml --all-targets -- -D warnings
```

The `smb-*` and `nova-*` binaries run campaigns or replay recorded streams.
They require the workload ROM and the matching QuickNES core. Campaign output
contains a recorded stream and checkpoint so a completed run can be replayed
without re-running the search decisions.

`searcher/src/nova/README.md` documents the source-built Nova workload and its
input and observation map. `machine/README.md` and `searcher/README.md`
describe the reusable interfaces.
