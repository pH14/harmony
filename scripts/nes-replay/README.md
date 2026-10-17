<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# NES replay in a second emulator

This tool replays Famidash victory tapes in any libretro NES core. It checks
that a win the searcher found under our QuickNES build also wins in another
emulator. The check reads the game's own completion state: Famidash writes 3 to
the game state byte at `$0498` when a level is complete.

`libretro_host.py` is a small headless libretro frontend written with ctypes.
It loads a core, loads a ROM, feeds one controller byte per frame, and reads the
2 KiB of system RAM after every frame. `famidash_replay.py` uses it to repeat
the Famidash workload's level start and play a tape.

The tool is a manual check. It is not part of CI.

## Inputs

- A libretro core library. Three are useful:
  - QuickNES at the revision the workload pins, built with
    `scripts/build-quicknes-core.sh OUTPUT`. This core validates the host: every
    tape must complete at its recorded frame.
  - libretro-fceumm, built from source at a pinned commit:

    ```sh
    git clone https://github.com/libretro/libretro-fceumm.git
    git -C libretro-fceumm checkout --detach 7a542dab1e87679921962a9f056186eca425c0c2
    make -C libretro-fceumm -j8 DEBUG=0
    ```

  - Mesen, from the libretro fork at a pinned commit:

    ```sh
    git clone https://github.com/libretro/Mesen.git
    git -C Mesen checkout --detach 0102910c39ad1a62bc3f784466f3f67ca9eae335
    make -C Mesen/Libretro -j8
    ```

- The four Famidash 1.2.8 ROMs in one directory: `famidash.nes`,
  `famidash-b-sides.nes`, `famidash-c-sides.nes`, `famidash-d-sides.nes`.
- `nes-eval` run directories for Famidash. Each holds `identity.json`, whose
  emulator backend names the genesis (`genesis=famidash-b-level-4-to-4-v1`), and
  `victory-input.json`, the victory tape as `{buttons, hold_frames}` actions.
  Only single-level genesis is supported.

## Usage

```sh
scripts/nes-replay/famidash_replay.py replay \
    --core fceumm_libretro.dylib --boot-frames 1 --quicknes-power-on-ram \
    --rom-dir ROMS --screenshots shots --output fceumm.jsonl --quiet RUNS_DIR

scripts/nes-replay/famidash_replay.py diverge \
    --core-a quicknes_libretro.dylib \
    --core-b fceumm_libretro.dylib --boot-frames-b 1 --quicknes-power-on-ram \
    --rom-dir ROMS RUNS_DIR/d-l4-s2
```

`uv` installs Pillow on first run. `replay` runs one process per tape, because a
libretro core keeps global state. It prints one JSON line per tape and a
summary. Each line has the recorded frame, the first frame `$0498` reads 3, the
first frame the workload's decoder would call a death, and whether the
completion frame equals the recorded frame. With `--screenshots` it saves the
completion frame as `RUN.png` and the level-complete banner 300 frames later as
`RUN-banner.png`.

The host calls `retro_init` before it sets the frame callbacks, because Mesen
needs its console object before it accepts them. Each core gets an empty
temporary system and save directory, so no saved game carries over between
runs.

## How a tape is played

The host repeats `FamidashTarget::from_power_on` from
`workloads/nes-games/src/famidash/target.rs`:

1. Load the ROM and fill the core's save RAM with `$FF`.
2. Play the level-select inputs: 22 idle frames, A for 1 frame, 936 idle
   frames, then Right for 1 frame and 20 idle frames for every side except main
   levels 0-21, then A for 1 frame and 90 idle frames.
3. Write the level number to `$047B` if it differs.
4. Press A for 1 frame and wait 40 idle frames. This is genesis, frame 0.
5. Play the tape. Each action holds its buttons for `hold_frames` frames,
   clamped to 1-120.

Controller bytes use NES order from bit 0: A, B, Select, Start, Up, Down,
Left, Right. Libretro joypad ids 2-7 match bits 2-7. A is id 8 and B is id 0.
The workload stops a tape at the first terminal frame and shortens the last
action to end there, so the recorded victory frame is the sum of the tape's hold
frames.

QuickNES and fceumm number power-on frames differently. fceumm reaches each
boot step one `retro_run` later than QuickNES, so without an offset the first A
press lands before the title screen accepts it and the setup never reaches
gameplay. `--boot-frames 1` runs one idle frame before step 2. Mesen needs no
offset.

Power-on RAM differs too. QuickNES fills it with `$FF` except `$08`, `$09`,
`$0A` and `$0F`, fceumm fills it with `$FF`, and Mesen with `$00`. Famidash
keeps some of those bytes into gameplay, so the fill changes which frames lag.
`--quicknes-power-on-ram` writes QuickNES's contents into the core's RAM before
the first frame, so the comparison does not depend on the fill.

## Divergence report

`diverge` runs core A and core B in lockstep in one process on one tape and
compares RAM after every frame. The stack page `$0100-$01FF` is left out,
because interrupts land on different instructions in different emulators. The
report lists the RAM bytes that differ at genesis, how each core ended, and the
first lasting gameplay divergence. The gameplay fields are the game state,
level, gamemode, cube data, player position and vertical speed, and the scroll.
A divergence starts at a frame where those fields differ on at least 8 of the
next 16 frames, which skips one-pixel scroll differences that resolve.
`core_b_frames_ahead` is 1 or -1 when core B's horizontal scroll equals core A's
from one frame later or earlier. Famidash scrolls a fixed distance per gameplay
frame, so that offset means one emulator ran a lag frame the other did not.

A lag frame is a frame where the game's work runs past the next vertical blank,
so the game skips one gameplay update. Some Famidash frames finish within about
one CPU cycle of that limit. Small differences in emulated CPU timing, such as
sprite DMA taking 513 or 514 cycles, decide whether those frames lag, so a tape
timed for one emulator can be a frame off in another.

## Metroid

The same check works for Metroid on a machine that holds the ROM. The host
needs three changes, all taken from `workloads/nes-games/src/metroid/target.rs`.

- ROM image: set the battery bit (`0x02`) in iNES header byte 6 before loading,
  as `nes::with_cartridge_ram` does. The core then exposes cartridge work RAM
  `$6000-$7FFF` as libretro save RAM. Fill it with `$FF` after load.
- Start: there is no RAM write. From power-on, run `power_on_walk`: 300 idle
  frames, then twice Start for 4 frames and 180 idle frames. Then run single
  idle frames until the game mode at `$1E` reads 3 (in play) and health is
  full: BCD `$03` at `$0107` and `$00` at `$0106`. That frame is genesis. Count those extra idle frames in each core,
  since the two emulators can need different counts.
- Completion: the ending flag is cartridge RAM offset `0x883`, CPU address
  `$6883`. Victory is the first frame it is nonzero. Death is health 0.

Use tapes from new-game Metroid `nes-eval` runs: `victory-input.json` for a
win, or a first-discovery tape under `milestone-inputs/` for a named milestone
such as a boss defeat. A tape from a rooted run starts at a checkpoint, so it
needs that run's prefix tape played first.
