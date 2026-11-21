<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Twin Dragons workload adapter

This module adapts [Twin Dragons](https://github.com/Garydos/twin-dragons-nes),
a three-level NROM platformer by Garydos, to Dissonance's search interfaces.
Twin Dragons is a game declared through the searcher's assertion catalog:
`nes-games/src/twin_dragons/game.rs` holds the RAM decoder, power-on setup,
genesis check and declarations, and `nes-games/src/twin_dragons/twin_dragons.toml`
is the strategy that keys the archive. `TwinDragonsWorkload` is
`NesWorkload<TwinDragons, QuickNesMachine>`, the shared workload described in
the [`nes-games` README](../../../nes-games/README.md).

## ROM

`scripts/build-twin-dragons-rom.sh` downloads the upstream archive pinned in
`twin-dragons-versions.env`, checks its SHA-256 and the SHA-256 of the release
ROM it contains, and writes `build/twin-dragons/twin-dragons.nes`. The upstream
README reserves all rights to the sound effects, so the recipe zeroes the
FamiTone effect streams at CPU `$ECF0`-`$F9F9`. FamiTone stops an effect at its
first zero byte, so every effect is silent and the music, graphics and code are
unchanged. A walk-and-jump replay on both ROMs gives identical RAM outside the
stack and the sound driver's variables. The pinned ROM is the silent-effects
build; its hash is `TWIN_DRAGONS_ROM_SHA256`.

The ROM needs no toolchain. Rebuilding it from source would need Millfork 0.3.12
and a JVM; the upstream release ROM matches its committed CHR, music and
sound-effect binaries byte for byte.

## Memory map

The game is written in Millfork, so addresses come from the compiler and were
found by RAM probing and by reading the stores in the ROM.

| Address | Meaning |
| --- | --- |
| `$9E` | Game state: 0 title, 1 playing, 2 level-load screen |
| `$42` | Current level, 0 to 2 |
| `$35` | Next level: 1, 2, then `$FF`, which returns to the title |
| `$04E3` | Goal touched; the next level loads `$80` frames later |
| `$CE` | Transition timer |
| `$52`, `$57` | Camera screen and fine scroll; camera x is `$52 * 256 + $57` |
| `$62`-`$79` | Player struct: x `$62`, y `$63`, on ground `$6B`, facing `$74`, health `$75`, invincible `$76` |

World x is the camera x plus the player's screen x. The game has no vertical
scrolling. A state counts as loaded only when the next-level byte matches the
current level. While a level loads, the current level changes a few frames
before the goal flag clears, and the loaded check keeps that window from
counting as a second clear.

## Declared workload

Setup waits 60 frames, presses Start for 2 frames, and waits 240 frames. The
genesis check requires game state 1, level 0, next level 1 and health 3.

The controller vocabulary is the shared NES chord vocabulary without Start or
Select, with short holds of 2 to 12 frames and long holds of 6 to 40. The
campaign draws with the `energy_splice:6` mixture. Most coordinator jobs run a
single action, so a long hold of 48 frames or more walks into enemies and pits
before the search can react. Splicing archived tails gives some jobs the
dozens of actions that a jump sequence or a level load needs.

Health 0 is a terminal death, rejected by the strategy. The game would
restart the level, but a restart loses all progress, so the search ends the
rollout. Touching the goal of the third level is the goal. Earlier goals are
the `cleared_1` and `cleared_2` named progress marks, and `cleared_3`,
`victory` and `death` complete the marks.

The progress tier is the levels cleared, then the screen along the level's
route, so the newest level outweighs the levels behind it and a later screen
outweighs the screens behind it. In levels 1 and 2 the route runs right, and
the screen is world x over 256. Level 3's route runs right along the bottom of
the level to x of about 1,520, climbs to a corridor at the top (y below 64) at
the right end, and runs left along the corridor to the goal at x 32. Its
corridor screens count from the right, as screens 6 to 11, above the bottom's
screens 0 to 5. This route shape is game knowledge taken from winning runs
(ours): the bottom of level 3 never reaches y 64, and in recorded searches
every corridor state was entered at the right end. Ranking the corridor by x
alone puts the whole return walk below the right end; measured from power-on,
that tier gave no speedup, while the route-shaped tier cut the executions to
victory about fourfold.

During gameplay the place is the levels cleared and world x and y in 32-pixel
cells, the identity is the 16-pixel cell, each cell keeps one state, and higher
health is preferred. The title screen gets no key. The level transition takes
about 300 frames from touching the goal to the next level's first gameplay
frame. Its states are archived in the next level's tier at screen 0, since the
decoder reads no position outside gameplay, one place per 32 frames of the
transition timer `$CE`: four places while the goal is touched, one per stage of
the level-load screen, and one for the frames before the new level is loaded.
A job can then cross the transition in short steps.

## Running

```sh
workloads/nes/scripts/build-twin-dragons-rom.sh
```

`nes-eval` takes `"game": "twin-dragons"` with no stage, ai or whole-game
option, and `nes-film` renders its witness or champion inputs. `nes-core-oracle`
takes `twin-dragons` as a game name. The browser demo runs the same workload
over its WebAssembly core.
