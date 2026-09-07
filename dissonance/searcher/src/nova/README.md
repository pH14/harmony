<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Nova workload adapter

This module adapts Nova the Squirrel to dissonance's target-neutral search
interfaces. It owns Nova addresses, menu input, observations, archive keys,
state preference, and terminal conditions. The generic coordinator receives
only actions, ordered keys, observations, snapshots, and policy identifiers.

## Archive policy

Nova retains one scheduled representative per 16-pixel location. At the same
location, the adapter prefers states with more cleared levels, collectibles,
available levels, carried abilities, health, and puzzle chips, in that order.
Coarser archive groups represent durable progress and level identity.

The key sorts the ability, health, and chip fields ahead of level identity and
position. The archive reads a key's ordering as depth, so this ordering
disagrees with the declared grouping: the reported deepest key can move
backwards across a level once a run holds an ability at full health. The
disagreement is measured rather than accidental. Sorting progress first is the
tidier contract and searched worse — level 25 at seed 1 cleared in 104,751
executions under this order and had not cleared by 258,900 under the other,
because the splice donor gate stops preferring donors that reached a cell
without taking damage. `first_depth_ord_disagreement` pins the deviation so a
reorder has to be a deliberate re-measurement.

Reports may record progress reached inside an action. Reproducer selection uses
action endpoints, where the serialized input identifies the complete state.

## Reproducible inputs

`../../../../nova-versions.env` pins the Nova source revision, archive SHA-256,
and built ROM SHA-256. `../../../../scripts/build-nova-rom.sh` verifies the
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
| Puzzle chips/required | `$0508/$0509` | system RAM |
| Copied ability | `$7200` | save RAM `$1200` |
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
four A/B button states.

## Retention variant and replacement policy

The key names a location and the durable resources, so pose, momentum, and
the level's own actors are invisible to it. Two arrivals at one location are
then the same cell, and the cheaper one holds the only slot however badly it
is placed. `--fingerprint-bits` carries low bits of the work-RAM digest as the
key's variant, outside its identity and ordering, and `--replacement` says
when the archive reads them: `opaque_preference_then_fewest_frames` never
(the default, unchanged), `..._per_variant` at every slot from its first
arrival, `..._or_pressured_split:R,D` once a slot's representative has been
drawn `D` times with no retained child and `R` arrivals have lost to it.
Every choice is recorded in the stream header and resolved on replay.

Level 9 (world 2) is the measurement that motivated all of it. Its corridor
cell at x=208 took 123 selections under an undecayed frontier, produced 411
candidates with healthy frame counts, and had all 411 rejected as duplicates.
The results, seed 1, one setting for every level, against the level panel:

| setting | L1 | L17 | L25 | L33 | L9 |
| --- | --- | --- | --- | --- | --- |
| default (splice, bounded, decay) | 3,777 | 2,547 | 104,751 | 3,126 | no clear, stuck at 3341 px through 1.6M |
| + no frontier decay | 4,678 | 2,521 | 72,118 (seed 2: 59,310 vs 124,346) | 1,654 | reaches the wall at 344K, no clear |
| + 6 bits, split every slot | -- | -- | no clear by 194K, 111K entries | -- | no clear by 206K |
| + 6 bits, split every slot, no decay | -- | -- | no clear by 137K, 46K entries | -- | behind no-decay alone at 100K |
| + 6 bits, pressured split 64,8, no decay | 4,678 | 2,521 | no clear by 358K; seed 2 77,731 vs 59,310 | 1,654 | at the wall from 193K, no clear by 362K |

Splitting every slot did clear level 9 once, in 154,281 executions, on a
build where the variant still sat inside the key's ordering and reordered
splice donors by hash; on the corrected build the same setting had not
reached the wall by 206,000. Triggers that split only barren slots -- by the
incumbent's own draws, by rejected arrivals, and by both -- each fired where
the search was busy rather than where it was stuck, or reached the corridor
too late to matter, and cost level 25 on every seed. Displacing a barren
incumbent instead of splitting was worse than leaving it.

The setting with the best measured record across the panel is the plain one
with the frontier decay removed: faster on levels 25 and 33, level on 17,
slightly slower on 1, and level 9 unsolved. Level 9's wall is not a
retention problem the archive can buy its way past at a price the other
levels will pay; the unmeasured lever is the draw itself, the chord
vocabulary and suffix shape that never produce the input that crosses.

## Terminal predicate

A run records which state it stops on. `first_durable_level_clear` is the
default and the level panel's meaning: the run ends the moment it clears one
more level than its genesis holds. `every_level_cleared` ends only once all
`NOVA_CAMPAIGN_LEVEL_COUNT` levels are cleared, and under it a level clear is
an ordinary archived candidate, so the search carries on into the level the
clear opens. `nova-campaign --terminal` selects one; `--suffix` and
`--mixture` select the generic draw policies the same way `smb-campaign`
does.

A whole-game campaign needs `every_level_cleared`. The predicate also decides
whether a clear freezes the target: under the default `apply` and
`survives_probe` refuse to emulate past a cleared level, which keeps a
terminal state from drifting but leaves a post-clear state unable to advance
at all. Jobs selecting one emulate no frames, rebuild its key, and are
rejected as duplicates, so no execution budget, selector, or suffix shape can
move it. `every_level_cleared` lifts the freeze and lets the predicate alone
decide.

## Whole-game measurements

Seed 1, `every_level_cleared`, the ability-identity key, and the plain
no-decay selector (`..._energy_frontier_cheapest:3,4096,4096,4096`), one
setting held across the run:

| genesis | cleared | executions |
| --- | --- | --- |
| level 1 | levels 1, 2 | 9,900 and 30,500; level 3 uncleared at 334,500 |
| level 1, `has_ability` bool key | levels 1, 2 | level 3 uncleared at 516,400 |
| level 2 | levels 2, 3 | 2,500 and 15,000; level 4 uncleared at 36,400 |
| level 3 | levels 3, 4, 5 | 8,600, 11,800 (two); level 6 uncleared at 152,900 |

Level 3 in isolation clears in 8,596. Entering it with a carried ability and
extra collectibles is not the difference: the level-2 genesis clears it in
3,000 once it arrives. The level-1 genesis holds more entries, and takes more
draws, in the door room than the isolated run took in the whole level (305
against 52 by 110,000 executions), and never clears it.

The door sits at (2887, 136) on a one-tile platform between two pits and
needs a fresh Up press while standing there. Replaying the level-1 genesis
archive at 110,000 executions shows every representative in the door's cells
falling past the platform: the slot's fewest-frames representative is the
arrival that flew through, and no one- or two-action suffix from any of
them lands and presses. The isolated run's clear came from a cell above the
door with a two-action suffix, a long hold that dropped the player onto the
platform and then Left+Up; the level-1 genesis holds the same cells and did
not draw that pair. Each global lever tried on that genesis left level 3
uncleared: entry retirement 8 and 16 in place of 3 (217,200 and 220,300),
a frontier rank span of 8 in place of 16 (128,700), 6 fingerprint bits with
pressured split 64,8 (245,400), and 6 bits split at every slot, which had
not cleared level 2 by 183,100.

What the level-1 genesis lacks is a representative standing on the platform,
and no existing knob names one: the key sees location and resources, not
whether the arrival has come to rest. A settled-state preference at
admission, measured against the key rather than any Nova field, is the
generic lever this points at and is unmeasured.

## Replay and media capture

`nova-campaign` records and replays the campaign stream in its standard mode.
The `--marketing-soak` mode runs to victory or its execution cap, stores a
compact progress summary, and replays only the winning or leading input with
video and 48 kHz stereo capture enabled. Headless search keeps audio and video
disabled. The captured endpoint must match the headless decoded endpoint.

The Nova source is GPL-3.0-or-later. Its graphics, levels, and other assets use
CC BY-NC-SA 4.0 with additional upstream restrictions. Published media includes
`dissonance/NOVA-ARTIFACT-LICENSE.md`; ROM and emulator binaries are excluded.

## Local Linux run

```sh
sudo apt-get install cc65 ffmpeg
dissonance/scripts/build-nova-rom.sh dissonance/nova-build
scripts/build-quicknes-core.sh dissonance/nova-build/quicknes_libretro.so
cargo run --locked --release --manifest-path dissonance/Cargo.toml \
  --bin nova-campaign -- \
  --core dissonance/nova-build/quicknes_libretro.so \
  --rom dissonance/nova-build/nova.nes \
  --output dissonance/nova-artifact \
  --seed 1 --executions 500000 --workers 4 --action-limit 512
```
