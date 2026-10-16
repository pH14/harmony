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
| `$04A8` | Vertical scroll sub-pixel |

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
6 to 40. A death bit is a terminal death. The search plays the main levels in
order from the starting level, and the objective is game state 3 in Dash,
level 21. Frames outside gameplay are not archived.

Game state 3 in an earlier level ends the action at that frame. The target
then presses buttons to start the next level: A once every 16 frames until the
level-select state 6 appears, 20 idle frames, Right, 20 idle frames, A, and
the 90-frame wait from setup. It checks that the level number went up by one
and that gameplay is running. The advance takes about 290 frames. Its presses
are part of the recorded input, so replays and films follow the same path.
The action's observation list ends with the arrival in the next level.

The archive key is the level, world x and y, game mode, gravity, size, dual
mode, the sign of the vertical velocity and the camera's vertical sub-pixel
`$04A8` in eighths. The place is the level and the 32 × 16-pixel cell with the
mode fields, progress is the level and the 512-pixel band of world x, and the
identity is the vertical motion with the camera eighth. Each identity at a
place keeps one entry, the one furthest left. The level scrolls at a fixed
speed, so x is also time, and a later arrival in the same cell has had fewer
frames to line up the next jump. Many later arrivals are already doomed
whatever the player presses.

The camera sub-pixel changes only while the camera scrolls vertically, and
world y leaves it out. Collision uses the camera's whole pixel, and the game
drops the sub-pixel when the camera reaches the top of the level, which moves
the player by that fraction. Two states in the same cell can therefore reach a
wall or the top of the screen one frame apart. On one Clubstep route a resting
cube passes the looping shaft near x 8,450 only when the sub-pixel is 0x56 to
0x9A, and the value is set about 2,400 pixels earlier. Each camera eighth
keeps its own entry, so a later arrival with another value is kept.

Each progress band is one search tier. In Clubstep's shaft, from x 8,450 to
8,960, the stalled runs held only doomed states on the flipped-gravity route.
That route kept reaching new cells, so its tiers never went dry. The route that
passes leaves it near x 7,690 as a UFO in normal gravity. With 512-pixel bands
that branch is one or two tiers below the shaft and still gets draws.

The milestones are the levels cleared, the percent of the level being played,
the finished game and a death. The progress watermark orders levels cleared,
then percent, then world x.

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

Seeds 1 to 5 clear Stereo Madness in 7,700 to 10,600 executions, 1.4 to 3.7
seconds on 8 workers, and each victory witness replays. `nes-film` accepts
`--game famidash` with `--level` for a run started at another level, and
renders the witness.

`famidash-probe ROM LEVEL INPUT.json` replays an input from a level's start and
prints the endpoint. With `trace` it prints the decoded state every frame. With
`frontier FRAMES [OUT.json]` it searches every press or release per frame from
the endpoint, keeping one branch per vertical position, velocity, mode and held
button, and reports how many branches stay alive. "every continuation dies"
means the endpoint is already lost whatever the player presses. `OUT.json`
receives a surviving or completing input. Above 4,096 branches it keeps an
even spread of them. With `timing STRIDE HORIZON SAMPLES` it replays the input
and, every STRIDE frames, draws SAMPLES random inputs of HORIZON frames from
three vocabularies: the search's taps and holds, taps of 1 to 4 frames, and
one random press or release per frame. It prints how many of each stay alive,
and how many one- and two-frame shifts of a press boundary in the input keep
it alive. `HARMONY_QUICKNES_CORE` names the core.

The browser demo in
`demos/twin-dragons` runs the same workload and starts later levels by writing
the level number at level select.
