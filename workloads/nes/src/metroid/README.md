<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Metroid workload

This package runs Metroid through the campaign contracts. Execution,
controller interpretation, RAM decoding, setup, and outcome evidence stay in
`nes-workload`; archive search and route reuse stay in `searcher`. ROMs and core binaries are private local inputs.

The registered workload starts a new game through ordinary power-on menus.
Equipment, tanks, boss defeat flags, and the ending flag come from cartridge
work RAM. The decoder maps Samus's screen using name-table membership and scroll
direction, so camera coordinates do not masquerade as player coordinates.
From a door touch until the arrival state, while the door state at `$56` is
neither 0 nor 5, the decoder keeps the map cell of the last frame before the
touch. A touch in a vertically scrolling room flips the scroll direction, and
the screen decode can then name the screen above the one Samus stands in until
the room scroll starts. The decoder applies this rule and the absent boss
reading rule below to every frame, a rooted target's prefix included, so a root
decodes the same as an action that ends at that frame. All
source addresses and meanings are documented beside their constants in
[`nes-games/src/metroid/target.rs`](../../../nes-games/src/metroid/target.rs). Zero health is death; the ending flag is victory. The legacy terminal
identifier is `death_or_ending_v2`.

The default terminal policy is `death_or_bcd_underflow_or_ending_v3`, which also
marks decoded health >=8000 as terminal. The damage routine stores a BCD
subtraction before testing borrow and clearing lethal damage, and a frame
boundary can expose that intermediate value. Six energy tanks cap normal health
at 6999, so an underflowed reading is the largest health any endpoint can
report; because health is the last term of the missile-first preference and the
leading resource term of the health-first one, that endpoint takes a slot at its
location from every legitimate endpoint beside it.
Raw health stays unchanged for replay inspection. Decoding stops at the first
frame the policy calls terminal while the action itself runs to its end, so a
terminal endpoint holds an observation and an emulator state from different
frames. The search neither admits nor snapshots a terminal endpoint, so this
only reaches replay tools, and only when one is pointed at a policy its tape
was not recorded under. The `death_or_ending_v2` predicate is also selectable,
through the evaluator's `metroid_terminal` request field and
`MetroidGame::with_terminal_policy`. Stream headers carry the chosen identifier
and reject a mismatched replay context. Tapes recorded under that predicate
replay under it: `nes-progress` names it, and `metroid-film` defaults to it.

[`nes-games/src/metroid/archive.rs`](../../../nes-games/src/metroid/archive.rs)
records the experimental adapter policy explicitly. The place is
the area, the map cell, boss damage and the Zebetite hits still needed. The
holder identity within a place is the 16-pixel position bucket, posture and
door-transition state. Health and missile stock are same-slot preferences: the
first preference favors missiles and the second favors a balance of the two. The
key's tank count subtracts the 75 missiles each boss kill awards, so a kill
does not relabel every map cell the killer reaches as holding fifteen more
tanks than the cells beside it; the kill still counts through the item term. The Brinstar statue room rewrites both boss bytes to `0x82` when
Samus approaches the statues: bit 7 still counts as the defeat, and each raised
statue adds one more item. The raise opens the passage beneath the statues
without changing position, health or stock, so without that term the opened
state is a same-key duplicate of the entry that triggered it and is never
admitted. Item and tank counts describe discovered capabilities. The staged
plan below supplies the item order, the route cells and the Kraid's door
target; the key's other terms supply no item, room or route. Coverage and pickup
counters are reporting evidence across explored branches, not proof that one
trajectory achieved their union. The count representation and
lexicographic resource preference are policy tradeoffs, not true capability
or resource dominance.

## Actions

An action is a button chord or a missile volley, defined in
[`nes-games/src/metroid/action.rs`](../../../nes-games/src/metroid/action.rs).
Chords come from the shared NES chord draw in the
[package README](../../README.md#chord-draw): the 36 chords of nine D-pad
positions and four A/B states, with no tap button, so a chord never presses
Start or Select and B from a chord always fires the beam. A red door takes
five missiles, and a Select tap left missiles armed for every later B press,
so random play spent its stock on whatever it faced before it reached a door.
One draw in 12 is a volley. A volley fires only while something that takes
missiles is in Samus's room: a closed missile door, a boss with health left
or a Zebetite with hits left. Elsewhere it waits 12 frames, the short chord
hold, so a volley can no longer spend missiles in rooms where they open
nothing.

The door objects sit in the object slots at `$0380`, `$0390`, `$03a0` and
`$03b0`. A slot reads 2 while its door is closed and waiting for hits, the
byte at offset 7 is the door palette, which sets how many hits the door takes
(palette 0 takes 5 missiles, palette 2 takes 10, palettes 1 and 3 open to a
beam), and the byte at offset `0x0c` is its name table. A door is in Samus's
room when its name table matches hers at `$030c`. An opened red door is never
loaded as closed again, so it stops counting once opened.

A volley records only a random word and resolves against the decoded state
when it runs, so a recording replays from its actions alone. The count is the
word modulo the missiles held, plus one. The target presses Select until the
toggle at `$010E` reads armed, presses B on every other frame until the
missile count at cartridge `0x879` has dropped by the count or the room holds
no missile target any more, then presses Select until the toggle reads off. A
shot counts only when the count drops, so a shot the game refuses, such as one
from the morph ball, is not counted. Arming and disarming give up after 8
frames, since Select flips the toggle within one press. Firing gives up after
64 frames without a shot: the game refuses a missile while the last one is
still in flight, and a missile crosses the screen in under 64 frames. With no
missiles, or outside play, a volley waits 12 frames. A volley plans 12
frames: most volleys find no target and wait, and from room entries at the
three Brinstar red doors a volley averaged 13 to 15 frames.

The searcher owns the suffix draw and the retained-input table.
`nes-progress` reads chord-only tapes.

## Archive key

The progress tier is the pair (stage, objective) of a staged plan in
[`nes-games/src/metroid/plan.rs`](../../../nes-games/src/metroid/plan.rs),
declared through the searcher's `search::plan`.
The plan is an ordered list of stages from power-on to Kraid's defeat: the
Maru Mari, energy, the first missile tank, energy, the bombs, energy, Kraid's
door, energy, and Kraid. A state's stage is the first stage whose done test it
fails, so an item taken out of order skips its stage when the plan reaches it.
Each stage climbs one integer:

- An item stage, and the stage to Kraid's door, climbs the index of the map
  cell the state is in along a route of map cells to its goal. Off the route
  the key keeps the parent key's index through `complete`, so a side room
  keeps its parent's tier and walking back along the route lowers it. A route
  stage with no item to take, such as Kraid's door, is done once its last cell
  is reached, and stays done for every descendant.
- An energy stage climbs whole energy units and stays done after a later hit.
  The stages before the Brinstar routes are done at 64 energy: an enemy touch
  in Brinstar costs 8 energy (`$80` at `$95CE` in the Brinstar bank), and 64
  is one touch for each of the eight screens of the shaft at map column 11,
  which the missile and Kraid's-door routes climb. The stage before Kraid is
  done at 80 energy: a touch in Kraid's hideout costs 20 (`$02` at `$95CF` in
  the Kraid bank), and 80 is four touches. Full energy for the tanks held is
  out of reach at Kraid's door on the recorded route, so the stage asks for a
  few touches of margin.
- The boss stage climbs boss damage. It is done when Kraid is defeated, read
  from Kraid's own status byte: the key keeps the defeated bosses as bits (bit
  0 Kraid, bit 1 Ridley), because Ridley can fall first and a boss count would
  then complete this stage with Kraid alive.

The routes come from the ROM's own tables, read through the
[disassembly](https://github.com/nmikstas/metroid-disassembly/tree/4270d57f9468daebdeea485686e31e26218a780c):
the item positions in each area's special item table, the room number of each
map cell in `WorldMap`, each room's doors and their colour, and each room's
tiles drawn from the area's room, structure and macro tables. Within a
horizontally scrolling stretch Samus walks to the next map cell unless a door
stands between them; in a vertical shaft she climbs or drops to the next cell
where the upper room's bottom two tile rows and the lower room's top two tile
rows are walkable (tile `$A0` or above) in two neighbouring columns, and
leaves sideways only through a door; every door flips the scroll direction.
Blast tiles (`$80` to `$9F`) are open going up, since Samus shoots up, and
open going down only once the route's stage holds bombs. The missile route
avoids red doors, since they open only to missiles. Each route is a shortest
path in that graph. Elevators join Brinstar `(7,18)` to Kraid's hideout
`(7,20)`.

The place is the area byte, the map cell, boss damage and the Zebetite hits
still needed, so every hit on a boss or a Zebetite column opens a new place
whose draw count starts fresh, and a state that has hurt either never displaces
one that has not. Each damage level stays a separate place in the boss
stage's tier for that damage, and draws spread across the places of a tier. The
holder identity is the position bucket, posture and door state. Tanks,
missiles and health are the preferences that decide which state holds a slot,
in two orders: missiles before health, and health before missiles. Two places
in the same tier are peers whatever their area byte, map row or column. Mother
Brain's defeat counts as one more item in the preferences: in Tourian, her
status byte at `$98` reads 3 to 7, 9 or 10, or the high byte of the escape
timer at `$010B` reads anything but `$ff`.

Boss damage is how far a lineage has worn down the boss of its area.
In Tourian the coordinate reads Mother Brain's remaining hits while her
status byte at `$98` says she is in view (1 idle, 2 hit): 32 minus her hit
count at `$99`, the count she dies at. The status byte clears whenever Samus
is in the other half of her room and once her death sequence starts; her hit
count persists, so the reading falls as
missiles land on her and never rises. It counts four per hit, the damage one
missile does to Kraid or Ridley, so one missile is one boss-damage bucket in
every boss room. Her full health is the reading's ceiling: an execution's
highest reading is at least her full health whenever she has a reading, so a
lineage that leaves her room and returns ranks by her hit count again.
The Zebetite columns are part of the place: the key carries the hits still
needed on every live column slot, so a state that has hit a column is a
different place from one that has not. Without it, a state that fired a
missile into a column shares a slot with the state that did not fire, and the
preferences keep the one with more missiles. A column healing or respawning
moves the state to another place without ranking it above or below the rest of
Tourian. The game keeps five Zebetite slots at
`$0758`, eight bytes apart, with the slot's status at offset 0 (low nibble 1
alive, 2 destroyed) and its missile hit count at offset 3; a column dies at
eight hits while healing one hit every 64 frames it is not hit, and the game
loads the columns of each screen as Samus enters it. The state also counts the
destroyed columns for the `zebetite_destroyed` milestone.
The game keeps six enemy slots at `$0400`, sixteen bytes apart, with the current
hit points at offset `$0b` and a mini-boss mark in bit 6 of offset `$0f`; `$ff`
hit points mean the slot holds nothing that can be hurt. Cartridge RAM holds
each slot's status at `$6AF4` (0 when unused) and its enemy type at `$6B02`,
with the same spacing. A freed slot keeps its old hit points and mini-boss mark,
so the boss reading comes only from an in-use slot holding Kraid (type 8 in area
`$12`) or Ridley (type 9 in area `$14`). The machine records cartridge RAM from
`$6877` through `$6B52`, which holds every cartridge byte the decoder reads, on
every frame, because a boss can die and free his slot partway through an
action. The key carries the
remaining hit points and the highest reading over the execution's frames in
the current area, and the lineage carries the highest reading it has seen in
its current area. The damage is the difference between that highest and the
remaining hit points, rounded up to buckets of four, so any hit, such as a
one-point bomb hit on Kraid, raises the boss stage's objective. Each area
holds one boss, so the highest starts from zero when the area changes. A
reading that goes absent keeps its last value while Samus stays in the same
map cell outside a door transition, because a hit flashes Kraid's or Ridley's
slot empty for a few frames. Kraid and Ridley regain full health when their
room is re-entered. Mother Brain follows the same rule until
she is defeated. Her hit count persists while she is off screen, but keeping her
reading in other map cells spreads the boss tier's draws over the Tourian
corridor, where no missile reaches her. The reading also drops when the item
count changes, because a kill adds an item. With no reading the damage is
zero. Without the coordinate a state that has landed ten hits on
Kraid shares a cell with one standing in the doorway, and no ordering can
prefer the first.

The key declares two preferences. Both lead with items then tanks. The first
then ranks missiles before health. The second ranks by balance, the smaller of
the missile and energy fractions of capacity, then missiles, then health.
Energy capacity is the health cap for the tank count. A state with no missiles
or no missile capacity has balance zero, and two such states are ranked health
first. A location keeps the best state under each, so at most two, and one
state holds both places when it leads on both. The two disagree on a resource
trade: twenty of twenty missiles at a tenth of full energy takes the first, ten
of twenty missiles at half energy takes the second, and a route that needs both
resources keeps the balanced state beside the stocked one. The order is
recorded as `preference_policy`, apart from `key_policy`, so a search
checkpoint resumes under a revised order and re-ranks each location's holders
from their stored keys.

The key records missile capacity and energy tanks for the balance preference.
They stay out of the tier, the place, the holder identity and the key's own
order, so two states that differ only in capacity compare equal everywhere
else.

The primary progress watermark records equipment bit count **plus boss
defeats**, and missile capacity.
The `milestones.tanks` field combines missile capacity divided by five
and energy tanks (boss capacity bonuses also inflate that field), so it
can improve while the primary watermark stays fixed.
`milestones.areas` is an area bitset, not a count: decimal 3 has two area bits
set. Inspect the individual fields and the verified witness before calling a
run stalled or combining observations into one trajectory.

The live sidecar separately counts observed map cells with a fixed 32 KiB bitmap
over raw area identity and the 32x32 map coordinates. It includes living gameplay
observations across explored branches in this run, and does not claim that one
trajectory visited their union. This observation memory is outside the archive's
logical budget and included in process RSS. The counter never enters archive
keys, input selection, rewards, or deterministic reports; it remains cumulative
when the archive's novelty ledger is compacted.

The last progress record of a run carries `retained_diagnostics`, the
end-of-run census of the live archive.
`live_entries_by_map_cell` maps `area:map_x:map_y` to
`[entries, max health, max missiles, equipment union, selections]`. A milestone
list says the run reached something; this says where the archive still sits,
what the endpoints in each cell can do, and how often the selector drew there. A
cell whose equipment union lacks an item the route out of it needs is covered by
endpoints that can never leave, and the selection count separates a cell the
selector never drew from one it drew and got nothing from.
`live_entries_by_map_cell_and_equipment` splits the same census by the equipment
byte, mapping `area:map_x:map_y:equipment bits` to `[entries, selections]`.
Endpoints holding different equipment in one map cell sit in different progress
tiers and are drawn separately, so a cell's own totals cannot say whether the
endpoints that can open the next door are the ones the selector goes back to.
Both read only cached active endpoints, so they are lower bounds where snapshots
are missing.

Use the common [local evaluation runner](../../../../benchmarks/search/README.md)
for paired search comparisons and full small-campaign replay. `metroid-campaign`
also exposes the native experiment command.

`metroid-film` replays a recorded tape to video. A tape carries no policy
header, so it defaults to `death_or_ending_v2`, the predicate the recorded
tapes were made under, and takes `--terminal-policy` to name another.
`--set-resources HEALTH,MISSILES@ACTIONS` repeats a bounded resource
intervention at that action count, so an input searched from an intervened root
plays back as the searcher saw it. `MetroidTarget::diagnostic_set_resources`
writes only the two health bytes and the missile count, at a paused live
boundary, within the endpoint's own earned capacities. It verifies that no other
RAM byte, mechanical field, byte of the decompressed emulator state or the
frame clock moved, and rolls
both regions back when any check fails. It is a standalone diagnostic, never a
search action and never a generated witness, so a replay must record and repeat
it. The intervention point is validated before any output file exists, and a run
that ends before the point is reported as the unintervened run.

`metroid-map-probe` replays a tape and prints the map cell, resources and the
key's stage and objective at each action endpoint, each key completed from the
one before it. Like `metroid-film` it defaults to `death_or_ending_v2`
and takes `--terminal-policy`, so a recorded tape is not stopped early
by a predicate it was never recorded under. A campaign report names the areas a run entered and counts the
map cells it observed; neither says which cells a route crossed, so neither can
say which neighbour of a reached cell was never opened.

## Shared target

`nes-eval` game `metroid-strategy` runs `MetroidStrategyGame`, the `Metroid`
game on the shared `NesWorkload` target with no hand campaign, under
`metroid.toml` or a request's strategy file, with records when asked. The ROM
image gains cartridge RAM, as in the hand target, because the game declares a
memory window there. Its setup tape is the hand target's power-on walk followed
by the 261 idle frames the hand target waits on this ROM until play starts, so
both start from the same genesis. Its live view reads the same `View` as the hand campaign,
and its trail motion carries no facing, which the decoded state does not hold.

## Archive memory

History compaction starts once the archive's history estimate passes a quarter
of `memory_limit`, and that estimate counts each entry, cell and slot at the
size of its key type, so the key type is a decision input. In the hand campaign
the route key `MetroidArchiveKey` is 26 bytes and the strategy key
`AssertionKey` 32; an archive entry (`entry_bytes` over `MetroidAction`,
`MetroidMilestones` and `MetroidSnapshot`) is 112 bytes under the route key and
120 under the strategy key. A test in `nes-games/src/metroid/keying.rs` holds
these numbers. The two arms compact at different executions, through the entry
size and through the cell and slot maps, so an outcome that decides the Metroid
port is scored on the strategy-keyed arm.

Resident snapshots are charged by `snapshot_memory_charge`: the struct's size
plus its emulator state bytes. `MetroidSnapshot` is 80 bytes. The QuickNES
state it holds is 21,182 bytes, packed with LZ4 to a mean of 8,936 bytes over
the 64 snapshots of one replayed tape (H seed 742890759 to Kraid's defeat), a
charge of about 9,016 bytes per snapshot. The shared target's `NesSnapshot<S>`
is larger: it also holds the strategy's `Accumulator` and an observation with
the decoded state inline, and its charge adds the accumulator's heap bytes.

## Named milestone evaluation

`workload_diagnostics.named_progress` reports Morph Ball, Bombs, Long Beam,
High Jump, Screw Attack, Varia Suit, Wave Beam, and Ice Beam independently;
Brinstar, Norfair, Kraid's area, Ridley's area, and Tourian independently;
the Kraid door cell, the Kraid and Ridley rooms (a boss health reading above
zero in the boss's area), Mother Brain's room (her status byte reading in
the room or hit), a destroyed Zebetite column, and Tourian's row-7 corridor
at columns 5 and 8, its bottom row (row 11 or below) and that row at columns
8 and 4 or before as route markers; and
Kraid defeated, Ridley defeated, Mother Brain defeated, escape started, and
the ending independently. Mother Brain initialization is not defeat; its $98
state machine is interpreted only in Tourian gameplay. Brief defeat/escape
transitions are latched from each frame without adding search events. Area entry never
implies boss defeat. Equipment is a union of observed gear bits, so beams lost
or replaced later remain recorded. `max_missile_capacity` and `max_energy_tanks`
are separate maxima, not a combined pickup score. Capacity is not a pickup
count: a boss defeat grants 75 additional missiles of capacity.

Every named milestone has a `first_seen` entry. Null means not observed within
this run's scope and budget. `execution` is the first admitted execution that
observed it; `route_action_end_frame` is the action endpoint on that discovered
input, **not** cumulative emulator work. Cartridge RAM is read after a held
controller action, so this is not an exact within-action pickup timestamp.
The finite vocabulary bounds observer memory and the number of saved tapes.
These fields are reporting-only: they do not change keys, rewards, input draws,
champion ordering, or continuation scheduling.

The common runner saves each first-discovery input under `milestone-inputs/`
and replays it on two independent targets, checking both the named milestone
and the deterministic endpoint. Beside each first discovery the campaign keeps
three more tapes per milestone, `NAME-energy.json`, `NAME-missiles.json` and
`NAME-boss.json`: the living action endpoint that satisfied the milestone with
the most health (missiles breaking ties), the one with the most missiles
(health breaking ties), and the one that left the boss in view the least
health (missiles then health breaking ties; written only while a boss reading
is present), each rewritten whenever a later endpoint strictly beats the held
one. A first
arrival is usually drained, so a search rooted at it starts short of what the
archive already holds at that place; these tapes make the best-stocked arrival
available as a root. They are not verified by the runner. Its main champion/victory witness separately
reports one trajectory's named progress. Do not call a union over search branches
one successful playthrough. Discovery-tape verification is charged to the
verification phase; the bounded export cost during discovery is part of search.
A first named discovery reconstructs its tape regardless of output configuration,
so publication options cannot alter deterministic report counters.
The live observer includes only observations admitted in this run, not a restored
archive's complete history. A missing field in a report means **unavailable**,
not zero. A retained champion replay cannot establish everything a search
explored or prove that no other branch defeated a boss.

The names and boss flags follow
[`Metroid_Defines.asm`](https://github.com/nmikstas/metroid-disassembly/blob/4270d57f9468daebdeea485686e31e26218a780c/Source_Files/Metroid_Defines.asm),
with the defeat write in `Bank07.asm` at `LDD75`: `(InArea & 0x0f) >> 1`
stores 1 at $687B for Kraid and 2 at $687C for Ridley. The key reads Mother
Brain's remaining hits into Tourian's boss health at four per hit while her
status byte says she is in view, with her full health as the ceiling of the
highest reading. It keeps the live Zebetite columns' remaining hits in the
cell below the map cell, and carries a lineage's highest reading across map
cells within one area. Named progress
(`metroid-named-progress-v3`) records the Kraid and Ridley defeat flags, Mother
Brain's state, latched Tourian events and the destroyed Zebetite column, and
binds Mother Brain's room to her status byte. Route timestamps exclude genesis
setup. The replay probe reports action execution work and setup separately, and
probes and backend snapshot replay are outside the execution-work counter. The
campaign stream format is v5, the snapshot checkpoint format is v13, and the
full-result digest format is v9. The combined capacity score is a separate named score.

Retrospective replay, without submitting an existing solution to search:

```sh
cargo build --release --manifest-path workloads/nes/Cargo.toml --bin nes-progress
workloads/nes/target/release/nes-progress metroid CORE ROM searched-input.json > progress.json
```

The tool starts from ordinary new-game genesis, records named discoveries,
repeats the replay, and includes ROM/core/input hashes and the prepared native
execution digest in its v3 report. Earlier reports lack the dependency identity
and cannot be compared as the same replay. In its replay report,
`first_seen.execution` counts tape actions, not historical search executions.
For Mega Man 2, `nes-progress mm2 CORE ROM searched-power-on-prefix.json wily4`
checks that the retained tape still reaches that stage through the current
runtime; it is a compatibility fixture, not a fresh search result.
