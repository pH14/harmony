<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Ice Climber workload adapter

This module adapts Ice Climber (NES, NROM) to Dissonance's search interfaces. The
game logic lives in [`nes-games/src/ice_climbers`](../../../nes-games/src/ice_climbers);
this module re-exports it and adds the native film and endpoint replay. The layout
follows the other campaign adapters: `target` holds the RAM decoder, setup tape and
`Target` implementation, `archive` the key, milestones and controller vocabulary, and
`campaign` the `Workload` implementation.

The game has 32 mountains. Each mountain is eight floors of ice, a bonus stage, and a
results screen. The adapter plays one climber from power-on.

## ROM

The ROM is not redistributed. Callers pass SHA-256
`491898c9b0bf6b242d08bb0841141ca404f8e54383a1814cdb7feab451da8701` (24 KiB PRG, 8 KiB CHR,
24,592 bytes with the header). The pinned QuickNES core runs it.

## Memory map

All addresses are in system RAM. Each one was found by watching the byte over scripted
play from power-on and comparing it with rendered frames.

| Address | Meaning |
| --- | --- |
| `$0A` | PPU nametable select bits; bit 1 is the vertical nametable |
| `$0B` | PPU vertical scroll, 0 to 239 |
| `$13` | Camera scroll during the intro pan over the mountain, 0 to 239, 2 pixels per frame |
| `$20` | Lives; starts at 3 |
| `$3F` | Death timer |
| `$59` | Mountain, 0 for mountain 1; it increases when the results screen finishes |
| `$5A` | Phase: 0 while a mountain loads or a screen changes, 1 while the climber plays |
| `$64` | Climber x in pixels |
| `$66` | Climber y in pixels on the screen |

The first frame of play has `$64` = `$38`, `$66` = `$C0`, nametable 3 and scroll `$CC`.
The world height of the climber is the scroll (240 added when the nametable bit is set)
plus `$66`. Altitude is `WORLD_BOTTOM` minus that height, and each floor is 48 pixels.

## Setup tape

The tape waits 120 frames on the title screen, taps Start for 2 frames, then waits 780
frames. The intro pan, which shows the whole mountain, takes about 725 frames. A second
Start press pauses the game, so the search tape holds Start only in the setup. Poking
`$59` at the title screen selects the mountain; the browser uses this to draw each map.

## Declared workload

- Tier is the pair (mountains finished, floor climbed in this mountain). The search
  prefers a higher tier.
- Place is the tier level, phase, an altitude band of 16 pixels and a 32-pixel column.
- Within a place, the entry with the greater altitude is preferred.
- A state is dead once lives drop below 3. A dead state is terminal, so every search
  branch is a one-life climb.
- Victory is clearing mountain 32.
- Chords are the pressable NES button masks with the shared short and long holds.
