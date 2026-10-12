<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Twin Dragons workload adapter

This module adapts [Twin Dragons](https://github.com/Garydos/twin-dragons-nes),
a three-level NROM platformer by Garydos, to Dissonance's search interfaces. It
owns the memory decoder, the power-on setup, controller encoding, mechanical
observations, archive identity, milestones, terminal conditions, and QuickNES
replay and rendering. It implements `CampaignTypes`, `TargetExecution`,
`InputPolicy`, `Evaluation` and `Reporting` the same way the
[Super Tilt Bro](../stb/README.md) adapter does, including the shortened hold
recorded at a terminal frame inside an action.

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
Select, with short holds of 2 to 12 frames and long holds of 48 to 120.

Health 0 is a terminal death. The game would restart the level, but a restart
loses all progress, so the search ends the rollout. Touching the goal of the
third level is the objective. Earlier goals are milestones that count cleared
levels. Level-load frames are runnable but are not archived.

The archive key is the level, world x and y in 16-pixel units, and health. The
place is the level and the 32-pixel cell, the identity is the 16-pixel cell,
and higher health is preferred within a place. The progress watermark is the
pair of levels cleared and furthest world x.

## Running

```sh
workloads/nes/scripts/build-twin-dragons-rom.sh
cargo run --release --manifest-path workloads/nes/Cargo.toml --bin twin-dragons-campaign -- \
  --core QUICKNES_CORE --rom workloads/nes/build/twin-dragons/twin-dragons.nes \
  --output OUT --executions 100000 --workers 6
```

The command runs the campaign, replays its stream, checks that the replayed
report and checkpoint match, and renders `witness.mp4` from the objective
witness or the champion. `nes-eval` and `nes-film` accept `"game":
"twin-dragons"`.
