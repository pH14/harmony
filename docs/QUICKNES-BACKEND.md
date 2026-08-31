<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# QuickNES NES backend

The SMB campaign loads QuickNES directly through the libretro C ABI. QuickNES
is the sole live NES backend: there is no runtime backend selector and no
cross-core stream or checkpoint compatibility surface. Its streams use
`smb-quicknes-campaign-stream-v2`, its checkpoints use
`smb-quicknes-snapshot-checkpoint-v2`, and evaluator-private fixtures use the
`dissonance-fixture-*-v2` formats. Historical measurements from the retired
target remain experiment records, not QuickNES execution counts or fixtures.

The adapter pins QuickNES revision
`26bb785c9deddb66a17717b21bb4e328f03ade32`, `DEBUG=0`, `-O2`, the revision's
`GIT_VERSION`, and all libretro options in `machine::quicknes`. Every stream
records those choices and the exact shared object's SHA-256. Every persisted
state carries the revision, binary hash, and fixed state length and rejects
cross-core or cross-build restore.

QuickNES leaves the three bytes named `ppu_state_t::unused2` uninitialized but
includes them in every libretro state. The adapter parses the core's block
format, requires the one 52-byte `PPUR` block, and zeros only those three
semantically unused bytes before a snapshot can enter the search. Restore
requires that canonical form. The earlier experimental `HQNESST1` state is
explicitly incompatible; `HQNESST2` prevents its nondeterministic padding from
being mistaken for a current checkpoint.

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

QuickNES's libretro wrapper has global emulator and callback state. The adapter
therefore copies the exact shared object to a unique temporary pathname for
each machine, loads that private image with local symbol visibility, and
unlinks it immediately. Workers never share an emulator and no lock serializes
execution. Video and audio are hard-disabled, controller input is provided
synchronously, system RAM is read directly from the core's validated 2 KiB
block, and state uses fixed-buffer serialize/unserialize.

## Distribution license gate

The upstream repository's top-level `LICENSE` is GPL version 2, its core-info
metadata says LGPL-2.1-or-later, and its source contains both GPL-2.0-or-later
mapper files and LGPL-2.1-or-later emulator files. Harmony is
AGPL-3.0-or-later. Harmony therefore does not vendor or bundle the core.
Distribution of a QuickNES binary together with Harmony is blocked until an
appropriate licensing review resolves the mixed upstream notices and
GPL-version compatibility. Supplying a separately built core for internal
measurement does not remove that distribution gate.
