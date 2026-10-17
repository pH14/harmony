<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Famidash workload adapter

This module adapts [Famidash](https://github.com/tfdsoft/famidash) by TFDSoft,
an unofficial NES fan demake of RobTop's Geometry Dash, to Dissonance's search
interfaces. The game logic lives in `nes-games/src/famidash`; this module adds
the QuickNES type alias, films and endpoint identities. The adapter implements
`CampaignTypes`, `TargetExecution`, `InputPolicy`, `Evaluation` and `Reporting`
the same way the [Twin Dragons](../twin_dragons/README.md) adapter does.

## ROMs

`scripts/build-famidash-rom.sh` downloads the four official 1.2.8 release ROMs
pinned in `famidash-versions.env`: the main game and the B-Sides, C-Sides and
D-Sides. It also downloads the MIT license, the credits and the four music
metadata files from release commit 0df680d, and checks every SHA-256 value. It
writes `famidash.nes`, `famidash-b-sides.nes`, `famidash-c-sides.nes` and
`famidash-d-sides.nes` to `build/famidash` unchanged, with
`UPSTREAM-music-*.json5` naming the artist of each level's song. The game is
MIT licensed, copyright 2023-2024 TFDSoft.

The ROM is an MMC3 cartridge with four-screen nametables and battery-backed
cartridge RAM. The pinned QuickNES core could not restore four-screen states,
so `scripts/build-quicknes-core.sh` applies a second patch that reads the 4 KiB
nametable block in one call.

## Levels

A level is a side plus its index on that side's ROM. `levels.rs` in
`nes-games/src/famidash` holds each level's name and length and the five
groups the arcade shows, 109 levels in all:

| Group | ROM | Levels | Menu |
| --- | --- | --- | --- |
| Main | `famidash.nes` | 0 to 21 | Main levels |
| Community | `famidash.nes` | 22 to 42 | Community levels |
| B-Sides | `famidash-b-sides.nes` | 0 to 30 | Community levels |
| C-Sides | `famidash-c-sides.nes` | 0 to 24 | Community levels |
| D-Sides | `famidash-d-sides.nes` | 0 to 9 | Community levels |

The title menu's first button opens the main levels and the second opens the
community levels. On the side ROMs the first button only shows a notice, so
their levels come from the second. `Side::from_rom` names the side from the
ROM's SHA-256. Release 1.2.8 has no E-Sides ROM.

## Memory map

Addresses come from the cc65 debug symbols of four builds of commit 0df680d:
`make`, `make b-sides`, `make c-sides` and `make d-sides`. The builds are used
for their symbols only. All 587 RAM symbols have the same address in the four
builds. The main, B-Sides and C-Sides builds are byte-identical to the release
ROMs. The D-Sides build differs from its release ROM in 28 bytes of level data,
with the same RAM symbols and level length tables. Every address below was
checked in the symbols of all four builds, and `famidash-probe` decodes every
one at a level start on each of the four release ROMs.

| Address | Meaning |
| --- | --- |
| `$0498` | Game state: 2 playing, 3 level complete, 6 level select |
| `$047B` | Level index on the side's ROM |
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
| `$045E` | Speed portal index |

World x is the scroll plus the player's screen x. World y is the nametable row
times 240 plus the fine row plus the player's screen y. Level percent is the
scroll divided by one hundredth of the level's length, from the game's length
table, and stops at 99 until the level is complete.

## Declared workload

Setup presses A at the title, waits for the menu and presses A for the main
levels, or Right, 20 idle frames and A for the community levels. At level
select it writes the level number to `$047B` when the menu shows another
level, as the browser demo does, then presses A and waits 40 frames. For Stereo Madness that is 1,091 frames. The
genesis check requires gameplay at 0 percent in the chosen level. On all 109
levels the level loads 30 frames after A and the player first moves 57 frames
after A.

A `Stage` names the side, the starting level and the goal level, both in one
group. A `nes-eval` request's `stage` alone runs that one level, and the goal
is the same level. `whole_game` runs from `stage`, or the group's first level,
through the group's last level. The side comes from the ROM. The emulator
identity names the side, the start and the goal.

The controller vocabulary is A alone, with taps of 1 to 4 frames and holds of
6 to 40. A death bit is a terminal death once the player's screen x is past
`$20`. A scroll past the level's length, in hundredths, is also a terminal
death. Every level's end trigger sits at or before that x, and a trigger
fires only while it is on screen, so a run past it can never finish the level. The game clears death bits while the player is at screen x `$20` or
less. Slaughterhouse on the D-Sides sets one 18 frames after the level start
and the level goes on. The search plays the group's levels in order from the
starting level, and the objective is game state 3 in the goal level. Frames
outside gameplay are not archived.

Game state 3 in a level before the goal ends the action at that frame. The
target then presses buttons to start the next level: A once every 16 frames
until the level-select state 6 appears, 20 idle frames, Right, 20 idle frames,
A, and the 40-frame wait from setup. Level select keeps the menu the setup
chose, so Right stays within the group. It checks that the level number went up by one
and that gameplay is running. The advance takes about 240 frames. Its presses
are part of the recorded input, so replays and films follow the same path.
The action's observation list ends with the arrival in the next level.

The archive key is the level, world x and y, game mode, gravity, size, dual
mode, the speed `$045E`, the sign of the vertical velocity and the camera's
vertical sub-pixel `$04A8` in eighths. The place is the level and the 32 ×
16-pixel cell with the mode fields and the speed, progress is the level and
the 512-pixel band of world x, and the identity is the vertical motion, the
camera eighth and the 8-pixel slot of x within the cell. Each identity at a
place keeps one entry, the one furthest left. At one speed x is also time, and
a later arrival in the same slot has had fewer frames to line up the next
jump. Many later arrivals are already doomed whatever the player presses.

The slot keeps jump arcs apart. Two cubes that jumped 12 pixels apart pass
through the same cells at the same height and vertical speed, and land 12
pixels apart, often one on a platform and one off it. With one entry per cell,
the arc that jumped first holds every mid-air cell of both arcs, and the later
arc is dropped as a duplicate. On D-Sides Kratos the jump onto the platform
before the green orb at x 1,760 has a 2-frame window. In a seed 1 run, every
mid-air cell of the winning arc was held by an entry from an earlier jump.

Speed portals set `$045E` to the portal's index: 0 for the 1x portal, 1 for
0.5x, 2 for 2x, 3 for 3x, 4 for 4x and 5 for the slow portal. Routes split at a
speed portal that one jump touches and the next misses, and the two speeds then
meet different obstacles at the same x. On C-Sides Thermodynamix a ship that
passes 18 pixels above the 3x portal goes on at 2x and later reaches a place it
cannot pass whatever the player presses. A cell holds each speed separately,
so the leftmost entry at one speed does not displace the other.

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
{"game": "famidash", "stage": 0, "rom": "workloads/nes/build/famidash/famidash.nes",
 "core": "QUICKNES_CORE", "rom_sha256": "ROM_SHA256", "core_sha256": "CORE_SHA256",
 "seed": 1, "workers": 8, "executions": 2000000, "memory_mib": 2048,
 "window": 1, "wall_seconds": 300, "mixture": "alphabet_only",
 "verification": "witness"}
EOF
cargo run --release --manifest-path workloads/nes/Cargo.toml --bin nes-eval -- request.json OUT
```

Seeds 1 to 5 clear Stereo Madness in 7,700 to 10,600 executions, 1.4 to 3.7
seconds on 8 workers, and each victory witness replays. `nes-film` accepts
`--game famidash` with `--level` and an optional `--goal` for the run's stage,
and renders the witness.

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
