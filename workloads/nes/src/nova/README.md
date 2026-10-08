<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Nova workload adapter

This module adapts Nova the Squirrel to dissonance's target-neutral search
interfaces. It owns Nova addresses, menu input, observations, archive keys,
state preference, and terminal conditions. The generic coordinator receives
only actions, ordered keys, observations, snapshots, and policy identifiers.

A snapshot holds the emulator state, the last observation and the failure flag.
It carries no copy of work RAM, and a restore reads nothing from the machine,
because the Consonance backend serves reads only after a run.

## Archive policy

Nova retains one scheduled representative per 16-pixel location under each of
four preferences. The first prefers states with more cleared levels,
collectibles, available levels, health and puzzle chips, in that order. Each of
the other three ranks the count of one key color ahead of the first preference.
The progress tier is the number of levels cleared in campaign order, the
leading run of set bits in the cleared-level bitmap. A glitch reachable in play
can rewrite other bits of that bitmap, and a count of every set bit would rank
such a state above the level the run has reached. The place is the collectible and
available counts, the level identity, the boss-fight count, the puzzle state and
a two-bucket position; the holder identity is the exact position bucket and the
held ability.
In whole-game mode the selector puts most draws on the highest tier, so
places in levels already cleared stop taking most of the search. A level
campaign stops at its first clear, so it has one tier.

A boss fight keeps the player on one screen until the boss falls, so position
alone gives the search one place for the whole fight. The decoder reads the
boss's own counter, and every hit or defeated enemy opens a new place. Nova's
object table has 16 slots and the game reorders them every frame for sprite
flicker, so the decoder finds the boss by its object type in any slot:

| Boss | Object type | Count |
|---|---|---|
| Scheme Team | `BOSS_FIGHT`, F3 0 or 1 | enemies left to defeat, 12 or 10 minus `LevelVariable` |
| Jack Stone | `BOSS_FIGHT`, F3 2 | hits in VX high, 0..16 |
| Forehead Block Guy, MolSno, John | own type | hits in F4, 0..8 |
| Fighter Maker | own type | phase in F3 times 5 plus hits in F4, 0..15 |
| Final boss | own type | reflected hits in VX high, 0..20 |

The Scheme Team counter reads zero while the fight object is still in its
initial state, before the fight sets `LevelVariable`. The count is zero outside
a fight.

Puzzle levels block their exits with colored locks and chip sockets. A key
opens a lock of its color on touch, and a chip socket opens once the level's
chips are all collected, so a held key or chip changes where the player can go
without changing the position. Keys live on the inventory's
per-level page, which every level start clears; a slot holds the item type and
one less than its count. The decoder counts red, green and blue keys from that
page and reads the carried sun key separately. A lock uses up one key of its
color, so holding more keys of a color never closes a route, and a row of locks
of one color needs as many keys. Held keys are same-slot preferences, as
Metroid's missiles are: the place leaves them out, and each per-color
preference keeps the state with the most keys of its color.

Nova holds one copied ability at a time, and it carries over between levels.
Abilities reach different spots: a burger ride climbs over a wall, and a
fireball launches metal arrows. Nova copies an enemy's ability only while holding
none, so swapping abilities passes through a state with no ability. The identity
keeps one holder for each ability at a place, so a state that swapped abilities
keeps its slot beside the states that did not.

Toggle switches flip one level-wide flag that swaps which toggle blocks are
solid, and a carried pickup block can be set down to bridge a gap. Both change
where the player can go at an unchanged position.

Arrow blocks are single-use. Touching a wood arrow turns the tile empty and
launches a flying arrow that the player can ride. A flying arrow empties the
next arrow, crate, bomb or fork tile it hits and turns or splits on arrows.
A state that has already spent the arrow a climb needs sits at the same
position as one that has not. The decoder counts the remaining wood, metal and
fork arrows (metatiles 41-44, 151-154, 157 and 158 in the source's
`metatileenum.s`) in the level map. Crates and bombs are left out, because
the player breaks crates in ordinary play and each broken crate would start a
new set of places across the whole level.

The puzzle state is the carried sun key, the carried pickup block, the toggle
flag, the puzzle chips and the arrow count. Every level load resets all of it
and the held keys.

Reports may record progress reached inside an action. Reproducer selection uses
action endpoints, where the serialized input identifies the complete state.

## Reproducible inputs

`../../nova-versions.env` pins the Nova source revision, archive SHA-256,
and built ROM SHA-256. `../../scripts/build-nova-rom.sh` verifies the
source archive, builds the ROM with cc65, checks its digest, and verifies the
observed linker symbols against the generated debug symbols. The QuickNES build
is pinned by the repository's `scripts/build-quicknes-core.sh`.

## Observation map

Coordinates use Nova's 12.4 fixed-point representation: `high * 16 + low / 16`.

| Observation | CPU address | Region |
|---|---:|---|
| Player X low/high | `$0025/$0026` | system RAM |
| Player Y high/low | `$0027/$0028` | system RAM |
| Health | `$004B` | system RAM |
| Internal/selected level | `$00A7/$00A8` | system RAM |
| Reload pending | `$00A9` | system RAM |
| Carried sun key | `$0500` | system RAM |
| Carried pickup block | `$0501` | system RAM |
| Toggle blocks enabled | `$0505` | system RAM |
| Puzzle chips/required | `$0508/$0509` | system RAM |
| Object type (16 slots) | `$002D` | system RAM |
| Level variable | `$038E` | system RAM |
| Object VX high (16 slots) | `$0423` | system RAM |
| Object state F2 (16 slots) | `$0463` | system RAM |
| Object F3/F4 (16 slots) | `$0473/$0483` | system RAM |
| Level map, 16 bytes per column | `$6000..$6FFF` | save RAM `$0000` |
| Copied ability | `$7200` | save RAM `$1200` |
| Per-level item types/amounts | `$720D/$7221` | save RAM `$120D/$1221` |
| Checkpoint level | `$7259` | save RAM `$1259` |
| Cleared levels | `$7F1F..$7F26` | save RAM `$1F1F` |
| Available levels | `$7F27..$7F2E` | save RAM `$1F27` |
| Collectibles | `$7F2F..$7F36` | save RAM `$1F2F` |

The addresses are linker symbols from the revision pinned in
`nova-versions.env`. The build rejects a ROM whose generated symbols differ.

## Setup and actions

The adapter performs a bounded title, main-menu, level-select, pre-level, and
gameplay sequence with release frames between edge-triggered presses. Genesis
is sealed after health and coordinates confirm gameplay. Search actions exclude
Start and Select. They combine nine non-conflicting directional states with the
four A/B button states. The adapter supplies that vocabulary, with no tap
button, to the shared NES chord draw in the
[package README](../../README.md#chord-draw); the searcher owns the suffix draw
and the retained-input table.

## Level and whole-game evaluation

The default level campaign stops on its first new durable clear.
`NovaGame::with_whole_game()` disables that intermediate stop and requires all
40 campaign levels to be cleared.

Whole-game mode plays the levels in the game's order. An exit door marks the
level cleared and selects the next level, then the game shows a level-end screen
and a pre-level menu whose level select can start any open level. The search
never drives those screens: after an action that clears a level, the adapter
waits out the level-end screen and presses Up three times and A on the
pre-level menu, the same presses the setup uses for the first level. Three Up
presses reach "Start!" from either initial cursor, because the menu has a fourth
"Show Intro" option, with the cursor on it, only for an uncleared level with an
intro cutscene. The adapter then checks that the game's checkpoint level is the
new level; a level that fails to start is an execution failure.
A state whose started level differs from the next level in campaign order ends
its job, as a death does. Play reaches another level only through a glitch,
which can open the level select after many deaths in one level. Without this
rule every earlier level would be a new place in the current tier.
The common `nes-eval` request selects this mode with `whole_game: true` from
level 1. Its fixed terminal policy is part of replay identity. Isolated later-
level fixtures initialize the declared prior-clear bitmap and remain separate
from fresh whole-game searches.

`NovaLevel` and the operator's `level` parameter are one-based. Decoded
`started_level` is the game's zero-based selected-level index; the current
internal map can differ when a level contains submaps.

## Replay and media capture

`nova-campaign` records and replays the campaign stream in its standard mode.
The `--marketing-soak` mode runs to victory or its execution cap, stores a
compact progress summary, and replays only the winning or leading input with
video and 48 kHz stereo capture enabled. Headless search keeps audio and video
disabled. The captured endpoint must match the headless decoded endpoint.

The Nova source is GPL-3.0-or-later. Its graphics, levels, and other assets use
CC BY-NC-SA 4.0 with additional upstream restrictions. Published media includes
`workloads/nes/NOVA-ARTIFACT-LICENSE.md`; ROM and emulator binaries are excluded.

## Local Linux run

```sh
sudo apt-get install cc65 ffmpeg
workloads/nes/scripts/build-nova-rom.sh workloads/nes/build/nova
scripts/build-quicknes-core.sh workloads/nes/build/nova/quicknes_libretro.so
cargo run --locked --release --manifest-path workloads/nes/Cargo.toml \
  --bin nova-campaign -- \
  --core workloads/nes/build/nova/quicknes_libretro.so \
  --rom workloads/nes/build/nova/nova.nes \
  --output /tmp/nova-artifact \
  --seed 1 --executions 500000 --workers 4
```

Execution work counts frames emitted by logical actions, including frames from
an action whose later save-RAM read or state decoding fails. Snapshot restore
and reset do not rewind this counter; setup and probes are excluded.
