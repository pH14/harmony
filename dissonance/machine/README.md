<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# machine

`machine` is the deterministic target boundary consumed by dissonance. The
`Machine` trait exposes snapshot, drop, branch, replay, run, and read
operations. Environments and decision answers are opaque byte blobs, so a
search policy can drive different machines without parsing their formats.

`StopReason` distinguishes terminal deadline, quiescence, and crash outcomes
from surfaced decision, snapshot-point, and assertion events. `StopMask` selects
which non-terminal classes a run returns; terminal classes always surface.
`Moment` and `DecisionId` identify points on one machine instance and are not
serialized as host timestamps.

`nes` defines the versioned controller-action reproducer format: each action is
`[buttons, hold_frames]`, with holds normalized to `1..=120` frames. The
searcher treats the resulting `Reproducer` bytes as opaque.

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

The Consonance adapter releases completed control-server virtual-time trace
segments at each successful branch or replay. Its public evidence is the action
observations and portable snapshot state; it does not expose accumulated exit
traces. Retiring these host-only buffers bounds memory across long campaigns
without changing guest state, snapshot bytes, or result digests. Direct
control-server users can still retain and export full session traces.
