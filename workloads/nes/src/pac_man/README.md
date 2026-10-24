<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Pac-Man workload adapter

This module adapts Namco's NES Pac-Man (NROM, 24,592 bytes) to Dissonance's
search interfaces. The game logic lives in
[`nes-games/src/pac_man`](../../../nes-games/src/pac_man); this module
re-exports it and adds the native film and endpoint replay. It follows the
other campaign adapters: a `target` with the RAM decoder, setup tape and
`Target` implementation, an `archive` with the key, milestones and controller
vocabulary, and a `campaign` with the `Workload` implementation.

## ROM

The ROM is not redistributed. Callers pass SHA-256
`a72c6f56b14be0f89ac08a3417b424565cdf0e7b53a501b786b25fa704643d80`
(24,592 bytes: iNES header, 16 KiB PRG, 8 KiB CHR). The pinned QuickNES core
runs it.

## Memory map

All addresses are in system RAM. They were read over scripted play from power-on
and checked against rendered frames and the game's disassembly.

| Address | Meaning |
| --- | --- |
| `$003F` | Mode: `$00` initialising, `$02` ready, `$04` play, `$06` ghost-eaten pause, `$08` death, `$0A` game over, `$0C` maze clear |
| `$0048` | Attract demo flag; non-zero means no player is in control |
| `$001A`, `$001C` | Pac-Man's x and y; the sprite's top left is `(x - 12, y - 4)` on screen |
| `$0051` | Heading: 0 up, 1 left, 2 down, 3 right |
| `$0067` | Lives, 3 at the start |
| `$0068` | Maze index, 0 for the first maze; `$FF` before the game starts |
| `$006A` | Dots left, 192 at the start of every maze |
| `$0070`-`$0075` | Score, one decimal digit per byte, least significant first, in tens |
| `$0088` | Power pellet active |
| `$0089`, `$008A` | Power pellet timer: seconds left and frames left in the second |
| `$060F` | Pause flag |

A maze is cleared when `$006A` reaches 0. The game then runs mode `$0C`, resets
to mode `$00` and `$02` with the maze index increased by one and the dots back
at 192.

## Declared workload

Setup waits 300 frames for the title, presses Start, waits 100 frames, presses
Start again and waits 252 frames for the ready sequence to end. The genesis
check requires mode `$04`, three lives, maze 0, no more than one dot eaten and
no power pellet.

The controller vocabulary is the four directions and neutral, with holds of 2
to 16 frames or 24 to 64 frames. Start would pause the game, so it is not in
the vocabulary.

Mode `$0A` is the terminal state: a job ends when the last life is lost.
No state is a victory, because the game has no ending.

Key choices:

- The progress tier is the number of mazes cleared, then the number of dots
  eaten in the current maze. Both only rise.
- The place is Pac-Man's 8 by 8 pixel cell and whether a power pellet is
  active.
- Only mode `$04` with dots left gets a key. Ready, death, maze clear and the
  ghost-eaten pause accept no input.
- The identity is empty and the capacity is one.
- The one preference is lives, then score.

Milestones are reported only and never enter the key: the first execution to
clear each of the first 32 mazes (`maze_1` to `maze_32` in
`named_progress.first_seen`) and the first game over. The arcade page groups
four mazes into each of its first five levels and the rest into the sixth.

## Running

`nes-eval` takes `"game": "pac-man"` with no stage, ai or whole-game option,
and `nes-film` renders its witness or champion inputs. `nes-core-oracle` takes
`pac-man` as a game name.

## Results

Native QuickNES on a 24-core lab machine, `nes-eval` with the live view and
witness verification. The witness is the champion tape, replayed twice in the
evaluator. Mazes cleared counts completed mazes in the witness.

| Run | Workers | Wall | Executions | Mazes (witness) | Mazes (any trajectory) | Score (witness) |
| --- | --- | --- | --- | --- | --- | --- |
| seed 1 | 2 | 900 s | 2.37 M | 22 | 23 | 63,830 |
| seed 2 | 1 | 1800 s | 3.04 M | 22 | 23 | 59,030 |
| seed 3 | 1 | 1800 s | 2.89 M | 22 | 22 | 56,210 |

The first 13 mazes fall in about a minute. Progress then comes in steps: the
search sits at the last few dots of one maze for several minutes and then
clears it and several more.

`tools/pac-man-replay.mjs` in `demos/twin-dragons` replays a `witness-input.json`
from power-on in the browser build of QuickNES and prints the mazes cleared and
score. Seed 2 and seed 3 end at the same maze count and score there as in the
native evaluator.
