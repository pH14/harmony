<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# gb-machine

`gb-machine` drives a Game Boy through the pinned libretro Gambatte core. It is
the execution half of the Pokémon Blue workload in [`../gb`](../gb): it loads a
ROM, runs button chords, snapshots and restores, and hands back work RAM. It
owns no game knowledge and no search policy.

It stands beside [`../nes-machine`](../nes-machine) rather than sharing its
types: the two have different RAM windows, different button sets, and different
snapshot hazards, and `nes-machine` also carries the NES Consonance backend.

`gb` defines the action format. Each action is `[buttons, hold_frames]` with
holds normalized to `1..=120` frames. The button byte is the Game Boy joypad
order: A `0x01`, B `0x02`, Select `0x04`, Start `0x08`, Right `0x10`, Left
`0x20`, Up `0x40`, Down `0x80`. The adapter maps these to libretro joypad ids.

## Gambatte adapter

`gambatte::GambatteMachine` loads a private copy of the pinned Gambatte shared
object, validates its library version and supplied SHA-256, and exposes the
8 KiB of Game Boy work RAM the core reports as system RAM. `read` addresses
that window at its hardware address, `0xc000..0xe000`. The core also exposes
cartridge SRAM; the workload does not need it and the adapter does not expose
it.

Search runs with audio and video disabled. Replay-only callers turn on video
and stereo PCM capture. Gambatte renders both regardless of what the frontend
reports, so turning capture on does not change what the machine computes.

Build the pinned core with [`../../scripts/build-gambatte-core.sh`](../../scripts/build-gambatte-core.sh),
which prints its SHA-256. The hash differs per architecture; callers pass the
hash of the file they loaded, and the adapter refuses a file that does not
match it. `HAVE_NETWORK=0` removes the serial-link listener and is part of the
library version string the adapter checks.

Headless throughput, one machine, video and audio off, measured by the
`headless_frame_rate` test:

| Host | Frames per second | 30-frame actions per second |
| --- | --- | --- |
| Apple M-series, macOS | 9,800 | 330 |

`run_chord` runs one chord and appends each frame's work RAM to `frames`;
`step_frame` runs a single frame, so a caller expanding a macro can read RAM
between frames. Both are action boundaries, which matters for the first hazard
below.

## Determinism

Two hazards sit between Gambatte and byte-exact replay, and the adapter closes
both.

**Host frame counters.** Gambatte's libretro layer counts emitted frames and
audio samples in host statics that are not part of the serialized state. When
the count of emitted audio samples runs ahead of the count of emitted frames,
`retro_run` repeats the last frame instead of emulating one, so what a call
computes depends on how many calls preceded it in this process.
`retro_unserialize` is the only thing that clears those statics. The adapter
therefore restores the current state before any action that did not already
begin with a restore, which makes an action's endpoint a function of its
starting state and its buttons alone. Without it, a suffix replayed from a
snapshot taken part way through it lands thousands of cycles from where the
same suffix landed when it ran in one pass.

**Peripheral records that carry host state.** Gambatte serializes the
real-time-clock and HuC3 records and the DMG palette table whether or not the
cartridge has those peripherals. On a cartridge without them the clock records
carry the host's `time(0)` at load and the rest carry uninitialized allocation
bytes; nothing reads them back. The adapter clears those records when it
captures a snapshot and refuses to restore one that still carries them, and it
refuses a cartridge type that has a clock — `0x0f` and `0x10` (MBC3 with a
timer) and `0xfe` (HuC3) — so the cleared records never hold live state.

Snapshots contain a format marker, the Gambatte revision, the core hash, the
fixed serialized-state length, and the canonicalized core state. Restore
rejects a different core build, a different state size, or a state whose
records do not tile the buffer exactly.

Gambatte's serialized state is a two-byte format version, an empty snapshot
block, then one record per field: a label ending in NUL, a 24-bit big-endian
length, and the payload. The adapter walks that list to find the records it
clears, and refuses a state whose records do not cover the buffer exactly.

The libretro FFI is Unix-specific. The pure state-walking and bounds checks and
the loopback core run without a shared object, including under Miri.

## Checks

```sh
cargo test --manifest-path workloads/gb-machine/Cargo.toml
cargo clippy --manifest-path workloads/gb-machine/Cargo.toml --all-targets -- -D warnings
```

The real-core tests are ignored by default. Run them against the pinned core
and the ROM:

```sh
HARMONY_GAMBATTE_CORE=/path/to/gambatte_libretro.so \
HARMONY_BLUE_ROM=/path/to/pokemon-blue.gb \
cargo test --release --manifest-path workloads/gb-machine/Cargo.toml \
  --test real_core -- --ignored --nocapture
```
