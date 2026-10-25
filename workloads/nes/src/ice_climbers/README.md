<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Ice Climber workload adapter

This module adapts Ice Climber (NES, NROM) to Dissonance's search interfaces. The
game logic lives in [`nes-games/src/ice_climbers`](../../../nes-games/src/ice_climbers);
this module re-exports it and adds the native film and endpoint replay. The layout
follows the other campaign adapters: `target` holds the RAM decoder, setup tape and
`Target` implementation, `archive` the key, milestones and controller vocabulary, and
`campaign` the `Workload` implementation.

The game has 32 mountains. Each mountain is eight floors of ice, a bonus stage, and a
results screen, after which the next mountain starts without input. The adapter plays one climber from power-on.

## ROM

The ROM is not redistributed. Callers pass SHA-256
`491898c9b0bf6b242d08bb0841141ca404f8e54383a1814cdb7feab451da8701` (24 KiB PRG, 8 KiB CHR,
24,592 bytes with the header). The pinned QuickNES core runs it.

## Memory map

All addresses are in system RAM. Each one was found by watching the byte over scripted
play from power-on and comparing it with rendered frames.

| Address | Meaning |
| --- | --- |
| `$13` | Camera scroll, 0 to 239. The intro pan lowers it 2 pixels per frame. In play it starts at `$9F` and drops 48 each time the camera climbs one floor, which happens after the floor counter reaches 4 and every floor after it |
| `$20` | Lives; starts at 3 |
| `$3F` | Death timer |
| `$59` | Mountain, 0 for mountain 1; it increases when the results screen finishes |
| `$E0` | Airborne flag: 0 on the ground, 1 while jumping or falling |
| `$5A` | Floor counter: 1 to 8 on the mountain, 9 in the bonus stage, 0 while a screen changes |
| `$64` | Climber x in pixels |
| `$66` | Climber y in pixels on the screen |

The first frame of play has `$64` = `$38`, `$66` = `$C0` and `$13` = `$9F`. The camera
top is 444 pixels at the start and 48 less after each camera step. The world height of the
climber is that top plus `$66`. Altitude is 636 minus that height, so each floor is 48 pixels.
The bonus stage is a separate screen drawn above the mountain on the map; its position is
`$66` alone.

## Setup tape

The tape waits 120 frames on the title screen, taps Start for 2 frames, then waits 780
frames. The intro pan, which shows the whole mountain, takes about 725 frames. A second
Start press pauses the game, so the search tape holds Start only in the setup. Poking
`$59` at the title screen selects the mountain; the browser uses this to draw each map.

## Declared workload

- Tier is the pair (mountains finished, floor climbed in this mountain). A jump rises
  51 pixels, three more than one floor, so an airborne state counts the floor below its
  altitude minus 4 pixels. Only a landing raises the tier.
- Place is the tier level, the floor counter, an altitude band of 16 pixels, an 8-pixel
  column and whether the climber is airborne.
- Within a place, the entry with the greater altitude is preferred.
- A state is dead once lives drop below 3. A dead state is terminal, so every search
  branch is a one-life climb.
- Victory is clearing mountain 32.
- Chords are the pressable NES button masks with the shared short and long holds.

## Floors and moving clouds

On the later mountains some floors have no platform. Clouds drift across those floors
and carry the climber, and the next full platform has holes the climber jumps through. The
climber must stay inside a hole's width for the whole rise, so the position that matters
is a few pixels wide. A 32-pixel column kept one arbitrary
position per cloud ride; an 8-pixel column keeps the positions that line up with a hole.

## Rooted runs

`nes-eval` takes `root_input` for this game: a JSON input whose actions are played after the
setup tape, and the search starts from the state they reach with an empty archive. The root
must end alive, with three lives, in play. The identity records a digest of the root actions.

## Power-on results

Three searches from power-on with the landed-floor tier, the 8 pixel column, the airborne place bit
and the broken-block identity (seeds 21 to 23, 5 workers each) all clear mountain 32 and report
victory after two witness replays: 6.9M, 10.1M and 11.3M executions, 29 to 50 minutes. A tape is
the setup tape followed by the witness actions. Each tape replays from power-on in a fresh
`castlevania-probe` process and ends with `$59` = 32 and `$20` = 3.

## Replay check

`nes-eval` verifies each witness twice in its own process. `nes-film` replays the same input in a
fresh process and reaches the same mountain and floor, but its endpoint snapshot hash differs from
the one `nes-eval` recorded, so `nes-film` stops at its endpoint comparison. The Zelda adapter shows
the same difference. `castlevania-probe` replays a tape from power-on in a fresh process and prints
`$59` and `$20`; it shows the mountain byte rising at the end of the champion tape.
