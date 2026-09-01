<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Running the SMB workload with QuickNES

QuickNES and the SMB ROM are external workload inputs. Harmony carries only
the libretro adapter and a reproducible build helper; it does not vendor the
emulator or ROM. The SMB campaign loads a user-supplied QuickNES core directly
through the libretro C ABI. Its streams use
`smb-quicknes-campaign-stream-v2`, its checkpoints use
`smb-quicknes-snapshot-checkpoint-v3`, and evaluator-private fixtures use the
`dissonance-fixture-*-v2` formats. Historical measurements from the retired
target remain experiment records, not QuickNES execution counts or fixtures.

The adapter pins QuickNES revision
`26bb785c9deddb66a17717b21bb4e328f03ade32`, `DEBUG=0`, `-O2`, the revision's
`GIT_VERSION`, and all libretro options in `machine::quicknes`. Every stream
records those choices and the exact shared object's SHA-256. Every persisted
state carries the revision, binary hash, and fixed state length and rejects
cross-core or cross-build restore. Loading also requires
`retro_get_system_info().library_version` to equal QuickNES's pinned
`CORE_VERSION` concatenated with the full revision, so a binary built from
another revision is rejected even when its hash is supplied.

QuickNES leaves the three bytes named `ppu_state_t::unused2` uninitialized but
includes them in every libretro state. The adapter parses the core's block
format, requires the one 52-byte `PPUR` block, and zeros only those three
semantically unused bytes before a snapshot can enter the search. Restore
requires that canonical form. `HQNESST2` is the only supported adapter state
magic; the normal revision and format checks reject other bytes.

Build an external core with:

```sh
scripts/build-quicknes-core.sh /path/to/quicknes_libretro.so
```

Run the campaign or benchmark with the external core:

```sh
HARMONY_QUICKNES_CORE=/path/to/quicknes_libretro.so \
HARMONY_SMB_ROM=/path/to/smb.nes \
cargo run --release -p searcher --bin smb-bench
```

For a bounded campaign, give the search archive an explicit deterministic
budget. The append-only stream remains authoritative; the in-memory archive is
a bounded breeding population and rebuildable acceleration structure. Reaching
the budget evicts the oldest selectable snapshots and compacts dead prefix
history without freezing admission:

```sh
HARMONY_QUICKNES_CORE=/path/to/quicknes_libretro.so \
HARMONY_SMB_ROM=/path/to/smb.nes \
cargo run --release -p searcher --bin smb-campaign -- \
  run genesis 6672613057367113729 4 2000000 512 laptop results/smb \
  --memory-budget-mib 2048 --no-final-artifacts
```

`--memory-budget-mib` is recorded in the stream, so competition and eviction
replay deterministically. History compaction also rebuilds remembered novelty
cells and pooled barren counters from the selectable breeding population, so
those ordered maps cannot grow with lifetime execution count. The progress
sidecar breaks the logical charge into snapshot, entry-metadata, shared-input,
novelty, barren-counter, and empirical draw-state bytes for budget audits. The
report-only progress curve also coarsens deterministically at 1,024 samples, so
it does not grow with campaign lifetime. `--no-final-artifacts`
suppresses only the final whole-population report and snapshot files; it still
writes the replayable stream, live progress sidecar, and throughput summary.
Use it for long runs to avoid multi-gigabyte completion writes. Checkpoints
from the previous snapshot layout are rejected by the v3 format rather than
interpreted as current state.

QuickNES's libretro wrapper has global emulator and callback state. The adapter
therefore copies the exact shared object to a unique temporary pathname for
each machine, loads that private image with local symbol visibility, and
unlinks it immediately. Workers never share an emulator and no lock serializes
execution. Video and audio are hard-disabled, controller input is provided
synchronously, system RAM is read directly from the core's validated 2 KiB
block, and state uses fixed-buffer serialize/unserialize. The QuickNES machine
boundary exposes only that 2 KiB work-RAM window through `Machine::read`, not
the NES CPU's complete 64 KiB address space.

## Behavioral differences from the retired backend

The pinned QuickNES core silently advances past illegal CPU opcodes. Its
internal `error_count` is not exposed by the pinned libretro ABI, so this
adapter has no sound signal from which to return `StopReason::Crash` for CPU
corruption. A heuristic based on game state would misclassify valid execution
and would not restore the retired backend's oracle. Campaigns therefore record
only machine failures that the ABI actually reports; illegal-opcode corruption
can remain live and enter the archive.

The frozen option `quicknes_up_down_allowed=disabled` also filters opposing
directions. In the default `nes_down_ten` vocabulary, mask `0xc1`
(A+Left+Right) consequently reaches the game as plain A. Changing the option
would change the frozen emulator identity and invalidate v2 fixtures, so a
future vocabulary revision must replace that dead entry instead.
