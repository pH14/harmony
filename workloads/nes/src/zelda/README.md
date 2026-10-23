<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# The Legend of Zelda workload adapter

This module adapts The Legend of Zelda (USA, Rev 1), the MMC1 (mapper 1)
release, to Dissonance's search interfaces. The game logic lives in
[`nes-games/src/zelda`](../../../nes-games/src/zelda); this module re-exports
it and adds the native film and endpoint replay. The layout follows the other
campaign adapters: one `target` with the RAM decoder, setup tape and `Target`
implementation, one `archive` with the key, milestones and controller
vocabulary, and one `campaign` with the `Workload` implementation.

The key is minimal on purpose. It holds no route knowledge, no item counts and
no per-room cases, so a run shows what the searcher finds in a game nobody
tuned. Each key choice and its reason is listed under
[Declared workload](#declared-workload).

## ROM

The ROM is not redistributed. Callers pass SHA-256
`44e33dc0a6fb663b98908ea761b33bbd77ca5092f25c624b10bf2a97e5406a5a`
(The Legend of Zelda (USA) (Rev 1), NES 2.0 header, 128 KiB PRG, CHR RAM,
battery-backed work RAM). The pinned QuickNES core runs it.

## Memory map

All addresses are in system RAM. The ones marked checked were read with
`mm2-probe` (it reads any ROM named by `HARMONY_MM2_ROM`) over scripted play
from power-on and compared with rendered frames. The others follow the
community RAM map and the game's disassembly; no scripted replay has reached
them yet.

| Address | Meaning | Checked |
| --- | --- | --- |
| `$0010` | Level: 0 overworld, 1 to 9 the dungeons | 0 only |
| `$0012` | Game mode: `$00` title, `$01` file select, `$0E` name registration, `$03` and `$04` level load, `$05` play, `$06` and `$07` screen scroll, `$0B` cave interior, `$10` stairs, `$11` death, `$13` ending | all but `$13` |
| `$0070` | Link's x in pixels | yes |
| `$0084` | Link's y in pixels; play spans about `$3D` to `$DD` | yes |
| `$00EB` | Room: the overworld screen (row in the high nibble, column in the low) or the dungeon room | yes |
| `$0657` | Sword: 0 none, 1 wooden, 2 white, 3 magic | 0 and 1 |
| `$0658` | Bombs | no |
| `$065A`-`$0666` | Bow, candle, recorder, food, potion, magic rod, raft, book, ring, ladder, magic key, power bracelet, letter | no |
| `$066D` | Rupees | no |
| `$066E` | Keys | no |
| `$066F` | Hearts: high nibble heart containers minus one, low nibble full hearts minus one | yes |
| `$0670` | Partial heart: 0 empty, 1 to `$7F` half, `$80` to `$FF` full | yes |
| `$0671` | Triforce pieces, one bit per dungeon 1 to 8 | no |
| `$0674`, `$0675` | Boomerang, magic boomerang | no |

At the first screen `$066F` is `$22` and `$0670` is `$FF`: three containers
and three full hearts. Damage takes `$0670` to `$7F`, then `$066F` to `$21`
with `$0670` back above `$7F`. The game enters mode `$11` on the frame the
last half heart goes, and mode `$08` after the death animation. The wooden
sword sets `$0657` to 1 on the frame Link touches it in the cave of the first
screen. During a scroll `$00EB` alternates between the two rooms for a few
frames.

## Declared workload

Setup waits 300 frames for the title, presses Start, presses Start on
REGISTER YOUR NAME, writes one letter with A, presses Select three times to
reach END, presses Start, presses Start on the new file, and waits 200 frames.
The genesis check requires mode `$05`, level 0, room `$77`, three containers
at full health, no sword, no key items, no Triforce pieces and no rupees.

The controller vocabulary is the stock one: every pressable mask (no Start,
no Select, no opposing directions) with the stock short and long holds, plus
a Start tap of 2 to 7 frames in one draw in 12. Start opens the item screen,
where the directions choose the B item, so a tap is how the search equips
bombs, the boomerang and the other B items.

Mode `$11` is a terminal death, as the other adapters end a job at the first
lost life. Mode `$13` is the victory: the ending after Ganon is defeated. No
run has reached it, so that reading is unchecked against the ROM.

Key choices:

- The progress tier is the number of Triforce pieces. It never falls, and the
  game needs all eight before Ganon. The sword, the key items and heart
  containers stay out of the tier, so the searcher has to find their value by
  itself.
- The place is the game mode, the level, the room and Link's position on one
  grid of 64 by 64 pixels (`$0070 >> 6`, `$0084 >> 6`: four columns and four
  rows of play). The grid is the same in every room. A whole room as one place
  hid the way out of a Metroid shaft, and 64 pixels is the coarsest size that
  still splits a room. The mode is in the place because a cave keeps the room
  byte of the overworld screen it opens from.
- The identity is empty and the capacity is one, so each cell keeps one state.
- The one preference is heart containers, then health in half hearts, then
  rupees. Ties keep the state with fewer frames.

Milestones are reported only and never enter the key: each Triforce piece by
dungeon, the best sword, the ten key items (raft, ladder, bow, candle,
recorder, power bracelet, magic key, magic rod, boomerang, book), the most
heart containers, victory and death. The first execution that reaches each is
reported as `named_progress.first_seen` (`triforce_1` to `triforce_8`,
`wooden_sword`, `white_sword`, `magic_sword`, the item names and `ending`),
which `benchmarks/search/milestones.py` reads. The progress watermark is
Triforce pieces, sword, key items held and heart containers, in that order.

## Running

`nes-eval` takes `"game": "zelda"` with no stage, ai or whole-game option,
and `nes-film` renders its witness or champion inputs. `nes-core-oracle`
takes `zelda` as a game name.
