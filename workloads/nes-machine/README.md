<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# machine

`machine` supplies execution drivers for the NES workload package. The
`Machine` trait exposes snapshot, drop, branch, replay, run, and read
operations. Environments and decision answers are opaque byte blobs, so a
NES adapter can drive either backend through the same execution vocabulary.

`StopReason` distinguishes terminal deadline, quiescence, and crash outcomes
from surfaced decision, snapshot-point, and assertion events. `StopMask` selects
which non-terminal classes a run returns; terminal classes always surface.
`Moment` and `DecisionId` identify points on one machine instance and are not
serialized as host timestamps.

`nes` defines the versioned controller-action reproducer format: each action is
`[buttons, hold_frames]`, with holds normalized to `1..=120` frames. The
NES targets interpret this format; Dissonance sees typed workload actions.

## QuickNES adapter

On Unix, `quicknes::QuickNesMachine` loads a private copy of the pinned
libretro QuickNES shared object, validates its revision and supplied SHA-256,
and exposes the core's 2 KiB system RAM. Search runs with audio and video
disabled. Replay-only callers can capture video and stereo PCM.

Snapshots contain a format marker, the QuickNES revision, core hash, fixed
serialized-state length, and canonicalized core state. Restore rejects a
different core build, state size, or non-canonical state. The adapter keeps
snapshots in an ordered handle table and resets staged input when restoring.

The libretro FFI is Unix-specific. The pure machine types and test loopback
allow the boundary and its bounds checks to run without a shared object.

The consonance adapter delegates boot, setup, branch, replay, run, read, SDK
catalog, and sparse snapshot operations to `consonance-client::Session`. Its
public evidence remains the action observations and portable snapshot state;
host-only control traces are not part of the machine contract. The adapter
keeps only NES publication discovery, billboard decoding, cached observation
state, and the workload-specific action payload codec.


## Consonance adapter

With the `consonance` feature on Linux x86-64 or arm64, the NES driver runs a
prepared ROM and QuickNES agent in one single-vCPU Consonance guest. It discovers
the publication by SDK names, validates the shared `nes-protocol` codec, and reads
RAM observations at stopped action boundaries. Game initialization and evaluation
belong to the adapters in `nes-workload`.

Setup and each action allow 2 seconds of virtual time on x86-64 and 20 seconds
on arm64. These architecture-specific budgets are passed through the client
session configuration and included in the execution identity.

Native snapshots preserve their existing serialized emulator format. Consonance
snapshots use the explicit `consonance-whole-vm-v2` shape owned by
`consonance-client`: setup base, execution identity, sparse pages, and opaque
sidecar bytes. Imports require the same execution identity; cross-backend
comparisons reconstruct state through actions. The sidecar uses portable
snapshot format 3, recorded separately in execution identity; older portable
formats are rejected explicitly.

Cartridge work RAM is exposed at `$6000` for workloads that need it. The
`nes::with_cartridge_ram` helper sets the iNES declaration on an in-memory
execution copy, preserving the original ROM identity. QuickNES's declared
cartridge RAM is initialized to `0xff` before gameplay, matching its undeclared
RAM initialization and preventing allocator contents from entering power-on
state. The same checked FFI path runs through the Miri loopback tests.

The source-built search CI also writes a pattern across declared cartridge RAM,
captures a real QuickNES snapshot, clobbers RAM, and restores it in both the same
core and an independent instance. Run this qualification locally with
`HARMONY_QUICKNES_CORE` and `HARMONY_NOVA_ROM` pointing at the pinned core and
source-built Nova image:

```sh
cargo test --locked --manifest-path workloads/nes-machine/Cargo.toml \
  --test cartridge_ram -- --ignored
```
