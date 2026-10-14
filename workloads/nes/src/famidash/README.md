<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Famidash workload adapter

This module adapts [Famidash](https://github.com/tfdsoft/famidash) by TFDSoft,
an unofficial NES fan demake of RobTop's Geometry Dash, to Dissonance's search
interfaces. The game logic lives in `nes-games/src/famidash`; this module adds
the QuickNES type alias, films and endpoint identities. The adapter implements
`CampaignTypes`, `TargetExecution`, `InputPolicy`, `Evaluation` and `Reporting`
the same way the [Twin Dragons](../twin_dragons/README.md) adapter does.

## ROM

`scripts/build-famidash-rom.sh` downloads the official 1.2.8 release ROM pinned
in `famidash-versions.env`, with the MIT license and the music credits from the
release commit, and checks all three SHA-256 values. It writes
`build/famidash/famidash.nes` unchanged. The game is MIT licensed, copyright
2023-2024 TFDSoft.

The ROM is an MMC3 cartridge with four-screen nametables and battery-backed
cartridge RAM. The pinned QuickNES core could not restore four-screen states,
so `scripts/build-quicknes-core.sh` applies a second patch that reads the 4 KiB
nametable block in one call.

## Memory map

Addresses come from the debug symbols of the 1.2.8 build.

| Address | Meaning |
| --- | --- |
| `$0498` | Game state: 2 playing, 3 level complete, 6 level select |
| `$047B` | Level, 0 to 42; the first 22 are the main levels |
| `$05B5` | Paused |
| `$7A` | Game mode: cube, ship, ball, UFO, robot, spider, wave, swing |
| `$7B`, `$7C` | Player flags; bit 0 is death, one byte per player |
| `$96` | Dual mode |
| `$043D`, `$0441` | Player x and y on screen, 8.8 fixed point |
| `$0449` | Vertical velocity, signed |
| `$044D`, `$044F` | Flipped gravity and mini size |
| `$04A2` | Horizontal scroll, signed 32-bit |
| `$04A6`, `$04A7` | Vertical scroll: fine row 0 to 239, then nametable row |

World x is the scroll plus the player's screen x. World y is the nametable row
times 240 plus the fine row plus the player's screen y. Level percent is the
scroll divided by one hundredth of the level's length, from the game's length
table, and stops at 99 until the level is complete.

## Declared workload

Setup presses A at the title and at the menu to reach level select, presses A
again to start Stereo Madness and waits 90 frames, 1,141 frames in all. A
request's `stage` starts another level instead: setup writes the level number
at level select before the last A press, as the browser demo does, and the
level is part of the emulator identity. The genesis check requires gameplay at
0 percent in the chosen level.

The controller vocabulary is A alone, with taps of 1 to 4 frames and holds of
6 to 40. A death bit is a terminal death. Game state 3 is the objective.
Frames outside gameplay are not archived.

The archive key is the level, world x and y, game mode, gravity, size, dual
mode and the sign of the vertical velocity. The place is the 32 × 16-pixel
cell with the mode fields, progress is the level and the 256-pixel band of
world x, and the identity is the vertical motion. Each place keeps one entry,
the one furthest right. The progress watermark is the best percent and the
furthest world x.

## Running

```sh
workloads/nes/scripts/build-famidash-rom.sh
cat >request.json <<EOF
{"game": "famidash", "rom": "workloads/nes/build/famidash/famidash.nes",
 "core": "QUICKNES_CORE", "rom_sha256": "ROM_SHA256", "core_sha256": "CORE_SHA256",
 "seed": 1, "workers": 8, "executions": 2000000, "memory_mib": 2048,
 "window": 1, "wall_seconds": 300, "mixture": "alphabet_only",
 "verification": "witness"}
EOF
cargo run --release --manifest-path workloads/nes/Cargo.toml --bin nes-eval -- request.json OUT
```

Seeds 1 to 5 clear Stereo Madness in 6,800 to 9,000 executions, about 2
seconds on 8 workers, and each victory witness replays. `nes-film` accepts
`--game famidash` with `--level` for a run started at another level, and
renders the witness. The browser demo in
`demos/twin-dragons` runs the same workload and starts later levels by writing
the level number at level select.
