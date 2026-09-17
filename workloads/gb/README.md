<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# blue-workload

`blue-workload` plays Pokémon Blue from a new game. The goal is the first gym
badge, Brock's Boulder Badge in Pewter City. It reads state from Game Boy work
RAM only and never decodes the screen. [`../gb-machine`](../gb-machine) runs the
emulator; this crate owns the game knowledge and the search policy.

Every address below was taken from the pokered disassembly
(https://github.com/pret/pokered) at `a1a22aa`, whose `pokeblue.sym` matches the
ROM byte for byte. Rust in this repository carries no comments, so the RAM map
lives here.

## Modules

| Module | What it owns |
| --- | --- |
| `target.rs` | the RAM map, state decoding, the macro actions, and `Target` |
| `map.rs` | the overworld collision model and its breadth-first routing |
| `archive.rs` | the archive key, the milestones, and the action draw |
| `campaign.rs` | the searcher contracts and the campaign entry points |
| `progress.rs` | the eight named milestones and when each was first seen |
| `film.rs` | the MP4 writer for a recorded tape |
| `fixtures.rs` | the two checked-in tapes |

## Work RAM map

Coordinates named `x` and `y` are player steps. One step is a 2×2 tile square.

| Address | Name in `wram.asm` | Read as |
| --- | --- | --- |
| `0xc100` | `wSpriteStateData1` | 16 sprite records of `0x10` bytes |
| `0xc109` | `wSpritePlayerStateData1FacingDirection` | player facing |
| `0xc200` | `wSpriteStateData2` | map position per sprite: `+4` y, `+5` x, both biased by 4 |
| `0xc3a0` | `wTileMap` | the 20×18 screen tiles, read by `blue-probe dump` only |
| `0xc6e8` | `wOverworldMap` | block ids, stride `wCurMapWidth + 6`, 3-block border |
| `0xcc24` | `wTopMenuItemY` | menu cursor row anchor |
| `0xcc25` | `wTopMenuItemX` | menu cursor column anchor |
| `0xcc26` | `wCurrentMenuItem` | menu cursor position |
| `0xcc28` | `wMaxMenuItem` | last selectable menu row |
| `0xcc29` | `wMenuWatchedKeys` | buttons the open menu accepts |
| `0xcc2d` | `wBattleAndStartSavedMenuItem` | battle menu cursor kept across turns |
| `0xcc36` | `wListScrollOffset` | first visible row of a list menu |
| `0xcd6b` | `wJoyIgnore` | buttons the game is discarding |
| `0xcf0b` | `wBattleResult` | how the last battle ended |
| `0xcfe6` | `wEnemyMonHP` | enemy HP, big endian |
| `0xd015` | `wBattleMonHP` | active party member HP, big endian |
| `0xd01c` | `wBattleMonMoves` | the four move ids of the active member |
| `0xd022` | `wBattleMonLevel` | active member level |
| `0xd023` | `wBattleMonMaxHP` | active member maximum HP, big endian |
| `0xd057` | `wIsInBattle` | zero outside a battle |
| `0xd059` | `wCurOpponent` | below 200 is a wild species, 200 and above a trainer |
| `0xd05a` | `wBattleType` | normal, old man, or safari |
| `0xd125` | `wTextBoxID` | `0x14` is `TWO_OPTION_MENU`, a yes/no box |
| `0xd163` | `wPartyCount` | party size |
| `0xd164` | `wPartySpecies` | species list, `0xff` terminated |
| `0xd16b` | `wPartyMons` | 44-byte records: `+1` HP, `+33` level, `+34` max HP |
| `0xd31d` | `wNumBagItems` | bag size |
| `0xd31e` | `wBagItems` | item id and count pairs |
| `0xd347` | `wPlayerMoney` | three binary-coded-decimal bytes |
| `0xd356` | `wObtainedBadges` | bit 0 is the Boulder Badge |
| `0xd35e` | `wCurMap` | current map id |
| `0xd361` | `wYCoord` | player step y |
| `0xd362` | `wXCoord` | player step x |
| `0xd367` | `wCurMapTileset` | tileset id |
| `0xd368` | `wCurMapHeight` | map height in blocks |
| `0xd369` | `wCurMapWidth` | map width in blocks |
| `0xd3ae` | `wNumberOfWarps` | warp count |
| `0xd3af` | `wWarpEntries` | 4-byte entries: y, x, warp id, destination map (`255` means the map you came from) |
| `0xd4b0` | `wNumSigns` | sign count |
| `0xd4b1` | `wSignCoords` | y and x pairs |
| `0xd4e1` | `wNumSprites` | how many sprite records are live |
| `0xd52b` | `wTilesetBank` | ROM bank holding the block data |
| `0xd52c` | `wTilesetBlocksPtr` | block data pointer |
| `0xd530` | `wTilesetCollisionPtr` | passable tile list in bank 0, `0xff` terminated |
| `0xd532` | `wTilesetTalkingOverTiles` | three counter tiles that double talking range |
| `0xd700` | `wWalkBikeSurfState` | on foot, on the bike, or surfing |
| `0xd730` | `wStatusFlags5` | includes the flag set while a script drives the player |
| `0xd747` | `wEventFlags` | 320 bytes of event bits |

Four event bits are read by number: got starter 34, Oak got the parcel 56, got
Oak's parcel 57, got the Pokédex 37.

## Overworld model

A map is a grid of blocks. Each block is 16 bytes holding 4×4 tile ids, and each
block is 2×2 player steps. `wOverworldMap` stores block ids with a 3-block border
on every side, so the row stride is `wCurMapWidth + 6`.

A step is walkable when the tile under the player's feet at the destination —
the lower-left tile of that step's 2×2 tile square — appears in the tileset's
collision list. `GetTileAndCoordsInFrontOfPlayer` reads screen tile (8, 9) for
where the player stands and moves two tile rows or columns from there, which is
that lower-left tile and not the upper-left one. Reading the upper-left tile
instead passes through ledges and walls that the game refuses.

Standing sprites are marked unwalkable, so a route never plans through an NPC.

Talking reaches one step. When the tile the player faces is one of the three
tiles in `wTilesetTalkingOverTiles` it reaches two, which is how the Poké Mart
cashier and the Pokémon Center nurse are reachable across their counters. The
alphabet generates both kinds of approach tile.

## Actions

Every action is a macro that the target expands into button presses, reading a
few RAM bytes between frames. `run_chord` is not used during a macro: the target
calls `begin_action` once at the action boundary and `hold_frame` per frame with
work RAM capture turned off.

| Macro | Parameter | Expansion |
| --- | --- | --- |
| `walk_to` | an alphabet destination | breadth-first route, one D-pad hold per step, stops on a battle, a map change, or a step that does not move; then turns to face, then presses outward if the destination is a map edge |
| `interact` | none | taps A up to 32 times, stopping on a battle, a map change, or a yes/no box |
| `battle_move` | move slot | FIGHT, then the move menu row, then runs the turn out |
| `use_item` | bag slot | ITEM, then the bag row, then runs the turn out |
| `switch` | party slot | PKMN, then the party row, then runs the turn out |
| `advance` | none | RUN in a wild battle, otherwise mashes B until control returns |

The battle menu is two columns of two rows: FIGHT and ITEM in the left column at
`wTopMenuItemX` `0x09`, PKMN and RUN in the right column at `0x0f`, with
`wMaxMenuItem` 1. The move menu that FIGHT opens is one-indexed: `wTopMenuItemY`
is `0x0c`, `wTopMenuItemX` is `0x05`, and `wCurrentMenuItem` 1 is the first move.
Selecting row 2 there picks the second move, not the third.

A drawn action names a macro and a slot. The alphabet is generated from the
current state, so a macro that the state cannot run is mapped onto one it can:
out of battle a battle move becomes a walk and an item or switch becomes an
interact, and in battle a walk becomes a battle move and an interact becomes an
advance. The slot is taken modulo the live alphabet size.

`action_cost_fn` is a pure function of the action, so each macro declares a frame
ceiling as its cost and stops when it reaches it. The frames actually spent are
reported separately as execution work.

| Macro | Frame ceiling |
| --- | --- |
| `walk_to` | 1500 |
| `battle_move` | 1200 |
| `use_item`, `switch`, `advance` | 900 |
| `interact` | 600 |

An action that changes map holds 150 idle frames afterwards so the new map is
loaded before the state is read. Those frames are held back from the macro's own
budget rather than added to it, so the ceiling bounds the whole action.

When the active party member faints and another is alive, the game opens a
replacement menu that B cannot leave. Every macro that waits on the battle menu
answers it: yes to the prompt, then the first living party member.

## Alphabet

In a battle: four move slots, up to four bag slots, one entry per party member,
and one advance.

Out of battle: every warp tile on the map, every approach tile for each sprite
and sign within six steps, one exit per map edge, and a straight-line walk of
one, two, or four steps in each direction.

## Adviser

`--advise` on `blue-campaign` puts the TypeSafe Jev decision model in the draw.
It needs `TYPESAFE_API_KEY`; without the flag nothing calls the network and the
run identity says `alphabet_adviser=none`.

The unit the draw weights is the macro kind. The kind is what a place decides:
in Oak's lab the route is walking and talking, on Route 1 it is walking and
leaving wild battles. The slot inside a kind stays uniform because a slot only
means something against the live alphabet, which the draw cannot see.

A place is the badges byte, the route flags, and the map id. The searcher hands
the draw a coarsened parent key through `duration_request`, so those three
fields are what the draw knows about where the lineage stands. Cells are left
out: a per-cell table would ask the model thousands of questions for one map.

The first draw at a new place uses even weights. `duration_request` records the
place, and at the next stream record `finish_stream_record` asks Jev one `choice`
question over the six macro kinds, each described by what it does in context. The
answer is a probability per kind, and each probability becomes a weight between a
floor of 16 and a ceiling of 256. A kind the model puts 0.9 on takes about three
quarters of the draws and every other kind keeps at least a twentieth. A `choice`
answer has to sum to one, which is why it is the question type here: the same six
kinds scored one at a time come back too close together to separate. A refused or
malformed answer stores even weights and counts a failure. At most eight places
are asked per record and the table holds 512 places.

The table goes into the campaign stream as a draw checkpoint at each record, and
a replayed advised run draws from the weights in that record. The campaign result
of an advised run is not replay-checked: the recorded table is what the draw
reads, and a replay would have to reach the API again to rebuild the same
counters. `--verify-replay` refuses `--advise` rather than compare two runs that
cannot match.

Map names come from `constants/map_constants.asm` in pokered. They are what lets
the model tell a house from a route.

## Campaign results

Three seeds, both arms side by side on one arm64 box at six workers each,
compared at matched executions.

| Arm | Seed | Executions | Maps | Milestones | Whiteouts |
| --- | --- | --- | --- | --- | --- |
| plain | 20260905 | 41,425 | 12 | 2 | 9 |
| plain | 20260906 | 40,276 | 12 | 2 | 8 |
| plain | 20260907 | 41,141 | 25 | 8 | 114 |
| advised | 20260905 | 47,609 | 12 | 2 | 250 |
| advised | 20260906 | 42,716 | 12 | 2 | 124 |
| advised | 20260907 | 45,981 | 12 | 2 | 522 |

The plain searcher wins the game on seed 20260907, taking the Boulder Badge at
execution 37,249 with a level 13 party. The badge crosses on one seed of three.

The adviser reaches Oak's parcel sooner on every seed and no further on any:
execution 994 against 1,407, 743 against 2,064, and 1,987 against 4,021. It also
whites out four to twenty times as often. The weight it puts on the fighting
kinds is spent losing wild battles rather than winning the trainer battles the
route needs, and the seed that won is the plain seed that fought the most.

## Archive key

Groups run coarse to fine: badges, then the seven route flags, then the number of
set event bits, then the stage of the fight, then the map id, then the 4-tile cell
of the player. `progress_cmp` ranks a group by how many badge bits it holds, then
by how many route flags, then by that event count, so a named milestone outranks
any amount of script progress, and neither the fight nor the position ranks at
all.

The event count is the game's own progress record and it is what makes a scripted
sequence visible. Oak's speech in the lab takes ten actions and sets three event
bits along the way. Without the count in the key those ten actions occupy two
slots, and the lineage that walks to the Poke Ball without hearing the speech holds
the slot that the lineage which heard it needs, so the starter is unreachable from
the slot that owns the tile.

The fight stage is 0 out of battle, and 1 to 8 in one as the opponent's remaining
health falls. It is what makes a fight winnable. The player does not move during
a battle and every turn costs health, so without it the eight actions of the
rival battle in Oak's lab share one slot, and the preference hands that slot back
to the lineage that has not fought. A plain run spent 59,517 executions inside
Pallet Town and its buildings for that reason; with the stage in the key a
12,000-execution run reaches twelve maps and Oak's parcel.

The stage is a place and not a rank, so a fight in progress never outranks a
milestone. It is dropped at the same depth as the map, so the pooled groups above
that depth still hold every lineage at a place whether or not it is fighting.

The preference inside a slot is party HP and then party levels. The plan asked
for fewest actions first; the archive already breaks a preference tie by lower
accumulated cost, and the key cannot see how many actions reached it, so the
resource preference is the key's part and the action count stays the archive's.

## Milestones

Eight bits in route order: got starter, got parcel, delivered parcel, got
Pokédex, entered Viridian Forest, entered Pewter, entered Pewter Gym, and the
Boulder Badge. Victory is the badge. The three "entered" bits come from map ids
rather than event flags, and every bit latches once set.

A whiteout — a party that is not empty with every member at zero HP and a real
maximum — is terminal and is never admitted. The game's blackout handler heals the
party before control returns, so the condition is latched the moment it appears
during an action rather than read from the state the action ends in. The maximum
HP has to be non-zero because a party record is filled over several frames when a
member joins, and reading it mid-write shows a member at zero.

## Fixtures

`fixtures/setup.json` is 379 button chords from power-on to Red's bedroom,
choosing the first preset name for the player and the rival.

`fixtures/brock.json` is 468 macro actions from that bedroom to the badge. It
takes Squirtle, wins the rival battle, fetches Oak's parcel from the Viridian
Poké Mart, delivers it, crosses Viridian Forest, grinds on Route 2 with trips to
the Pewter Pokémon Center, and beats the gym. `fixtures/brock-itinerary.json` is
the `blue-probe plan` itinerary that produced it, and `fixtures/brock-trace.json`
is the archive key and milestone flags after each of its actions, written by
`blue-probe trace`.

## Witnesses

`blue-campaign --output <dir>` writes `<dir>/milestones/<bit>-<name>.json` the
first time a milestone is reached, and `<dir>/champion-input.json` for the
deepest lineage. The bit prefix is there so the highest-numbered file in the
directory is the deepest milestone; the names alone sort the wrong way.

## Binaries

`blue-campaign` runs a campaign. `blue-film` renders a recorded tape to MP4 and
needs `ffmpeg`. `blue-probe` is the authoring and debugging tool: `setup`,
`state`, `dump`, `plan`, `route`, `trace`, `advise`, `alphabet`, and `shot`.
`advise <map> <route>` prints the weights Jev gives one place. `dump` prints the
walkability grid, the tile ids, the screen tile map, the warp table and the
sprite table; `shot` writes one frame as a PPM. `BLUE_TRACE=1` prints the decoded
state after every applied action.

## Scripts

`scripts/jev-offline.py` sends live archive entries from a campaign report to the
TypeSafe Jev decision model and scores its answers against the scripted route. It
asks one `choice` over the batch, one `score` per entry against the milestone
ladder, and one `noul` per entry for a dead end, all in a single request. It needs
`TYPESAFE_API_KEY` and reports cost and latency per batch.

```sh
uv run workloads/gb/scripts/jev-offline.py <run>/report.json \
  --trace workloads/gb/fixtures/brock-trace.json --out /tmp/jev.json
```

Archive entries are stored as a suffix on a parent, so the script walks the
parent chain to recover the actions and the frames that reached an entry. Entry
ids restart in every campaign, so the seed goes in the id the questions use.

Two orderings are compared against the route: Jev's score, and the archive's own
preference of badges, then route flags, then set event bits, then fewest frames.
Each is scored by the fraction of entry pairs it puts in the same order as the
scripted route.

A place on the route is the badges byte, the route flags, the map and the cell.
The route crosses Pallet Town and Oak's lab twice, so a place without the flags
would give a late revisit the depth of the early one.

## Checks

```sh
cargo test --manifest-path workloads/gb/Cargo.toml
cargo clippy --manifest-path workloads/gb/Cargo.toml --all-targets -- -D warnings
```

The route replay is ignored by default. Run it against the pinned core and the
ROM:

```sh
HARMONY_GAMBATTE_CORE=/path/to/gambatte_libretro.so \
HARMONY_BLUE_ROM=/path/to/pokemon-blue.gb \
cargo test --release --manifest-path workloads/gb/Cargo.toml \
  --test route -- --ignored --nocapture
```

A short campaign, which also checks that the recorded stream replays bit for bit:

```sh
cargo run --release --manifest-path workloads/gb/Cargo.toml --bin blue-campaign -- \
  --core /path/to/gambatte_libretro.so --rom /path/to/pokemon-blue.gb \
  --output /tmp/blue --executions 300 --workers 2 --verify-replay
```
