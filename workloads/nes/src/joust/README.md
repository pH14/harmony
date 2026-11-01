<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Joust workload adapter

This module adapts Joust (USA) to Dissonance's search interfaces. The game
logic lives in [`nes-games/src/joust`](../../../nes-games/src/joust): the RAM
decoder, power-on setup, observations, archive key, named progress, terminal
conditions and the `Workload` implementation. It follows the
[Castlevania](../castlevania/README.md) adapter for named progress and
checkpoint evidence. ROM files are never committed. The request pins the ROM
by SHA-256 `9c47a4797cdf3e46f068020ca6cd6ebfc5ee06d3083b12ded656051a1368ee4e`
(mapper 3, CNROM, 32 KiB of program ROM).

## Memory map

Addresses were checked with `joust-probe` on QuickNES from power-on: by tracing
every work RAM byte across a wave, by flying a bot that chases eggs and riders
from sprite memory, and by writing zero to the enemy counter to end waves.

| Address | Meaning |
| --- | --- |
| `$00`, `$02` | Game mode and phase. `$90` and 2 is play, `$80` and 3 is the game-over screen |
| `$1E`, `$1F` | Player y and x. y is larger lower, and x wraps at 256 |
| `$3A`, `$3B` | Wave number as two decimal digits, ones then tens |
| `$4E`, `$52` | Signed horizontal and vertical velocity |
| `$73` | Enemies still to be defeated in the wave |
| `$74` | Enemies spawned so far in the wave |
| `$75` | Enemies defeated in the wave |
| `$E9` | Lives in reserve; 4 at the start |
| `$EB`–`$ED` | Score as six BCD digits, low byte first |
| `$200`–`$2FF` | Sprite memory, used to count riders and eggs and to read the facing |

A rider is a group of four sprites with palette 2. Eggs use tile 204 with
palette 3. A sprite with the horizontal flip bit set faces left. Byte `$32`
changes between 0 and 1 during play and is not used.

## Declared workload

Setup waits 200 frames, presses Start for 3 frames, waits 60 frames, presses
Start again and waits 200 frames. The genesis check requires play mode, wave 1,
four reserve lives, no score and no enemies defeated. The game has no ending, so
the search target is a wave count, `TARGET_WAVE` (50), and the game-over screen is
terminal.

The controller vocabulary is the six held chords of Left, Right and neutral,
each with or without A, and a tap of A. A tap is a flap. Short holds are 1 to
10 frames and long holds 12 to 40.

## Archive key

The place is the wave and a 32 pixel cell of the player's x and y. Progress is
the pair of waves cleared and enemies defeated in the current wave. The
preference is more lives, then a higher score. A state on the game-over screen or
outside play has no key.

## Arcade view

`nes-games/src/arcade/joust.rs` shows one level per wave from 1 to 49; wave 50 is the victory wave. The cleared
count is the number of waves cleared. The browser builds each wave's map by
playing to that wave with the enemy counter zeroed, then hiding the sprites.
The score and lives counter are part of the map image.
