<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Metroid workload

This package runs Metroid through the campaign contracts. Execution,
controller interpretation, RAM decoding, setup, and outcome evidence stay in
`nes-workload`; archive search and route reuse stay in `searcher`. ROMs and core binaries are private local inputs.

The registered workload starts a new game through ordinary power-on menus.
Equipment, tanks, boss defeat flags, and the ending flag come from cartridge
work RAM. The decoder maps Samus's screen using name-table membership and scroll
direction, so camera coordinates do not masquerade as player coordinates.
While Samus touches a door, which sets bit 7 of the door state at `$56`, the
decoder keeps the map cell of the last frame before the touch. A touch in a
vertically scrolling room flips the scroll direction, and the screen decode
can then name the screen above the one Samus stands in. All
source addresses and meanings are documented beside their constants in
`target.rs`. Zero health is death; the ending flag is victory. The terminal
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

`archive.rs` records the experimental adapter policy explicitly. The place is
the area, the map cell, boss damage and the Zebetite hits still needed. The
holder identity within a place is the 16-pixel position bucket, posture and
door-transition state. Health and missile stock are same-slot preferences, and
the two preferences order that pair against each other in opposite ways. The
key's tank count subtracts the 75 missiles each boss kill awards, so a kill
does not relabel every map cell the killer reaches as holding fifteen more
tanks than the cells beside it; the kill still counts through the item term. The Brinstar statue room rewrites both boss bytes to `0x82` when
Samus approaches the statues: bit 7 still counts as the defeat, and each raised
statue adds one more item. The raise opens the passage beneath the statues
without changing position, health or stock, so without that term the opened
state is a same-key duplicate of the entry that triggered it and is never
admitted. Item and tank counts describe discovered capabilities; no
particular item, room, door target, or route is supplied. Coverage and pickup
counters are reporting evidence across explored branches, not proof that one
trajectory achieved their union. The count representation and
lexicographic resource preference are policy tradeoffs, not true capability
or resource dominance.

The adapter supplies its controller vocabulary as the alphabet sampler and
nothing else about drawing; the searcher owns the suffix draw and the
retained-input table.

The progress tier is the items held and whether the lineage has damaged the
boss in its room. Mother Brain's defeat counts as one more item: in Tourian,
her status byte at `$98` reads 3 to 7, 9 or 10, or the high byte of the escape
timer at `$010B` reads anything but `$ff`. The place is the area byte, the map
cell, boss damage and the Zebetite hits still needed, so every hit on a boss or
a Zebetite column opens a new place whose draw count starts fresh, and a state
that has hurt either never displaces one that has not. A damaged boss lifts
the state into the tier above its item count, so a fight in progress ranks
above the rest of the map, while each damage level stays a separate place in
that tier and draws spread across the levels. The holder identity is the position bucket, posture and door state.
Tanks, missiles and health are the preferences that decide which state holds a
slot, in two orders: missiles before health, and health before missiles. Two
places in the same tier are peers whatever their area byte, map row or column.

Boss damage is how far a lineage has worn down the mini boss sharing its room.
In Tourian the coordinate reads Mother Brain's remaining hits while her
status byte at `$98` says she is in view (1 idle, 2 hit): 32 minus her hit
count at `$99`, the count she dies at. The status byte clears whenever Samus
is in the other half of her room and once her death sequence starts, and the
reading is absent then; her hit count persists, so the reading falls as
missiles land on her and never rises. It counts four per hit, the damage one
missile does to Kraid or Ridley, so one missile is one boss-damage bucket in
every boss room. Her full health is the reading's ceiling: an execution's
highest present reading is at least her full health whenever she is in view,
so a lineage that leaves her room and returns ranks by her hit count again.
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
remaining hit points and the highest present reading over the execution's
frames, and the lineage carries the highest reading it has seen, so the damage
is the difference between that highest and the remaining hit points in buckets
of four. A reading of nothing keeps the parent's damage, because a hit flashes
the slot empty for a few frames. A lineage that leaves the boss's room, reading
nothing in a different map cell, starts from zero, because the boss regains
full health when the room is re-entered. The damage also starts from zero
when the item count changes, because a kill adds an item and the next boss is
untouched; without that, a lineage that killed Mother Brain would carry her
damage into the escape. While the reading stays present the
highest carries across map cells, so Mother Brain's reading ranks both
screens of her room on the same ladder. Without the coordinate a state that has landed ten hits on
Kraid shares a cell with one standing in the doorway, and no ordering can
prefer the first.

The key declares two preferences. Both lead with items then tanks; the first
then ranks missiles before health and the second health before missiles. A
location keeps the best state under each, so at most two, and one state holds
both places when it leads on both. The two disagree only on a resource trade:
ten missiles at twenty health takes the first, five missiles at two hundred
health takes the second, and a route that needs the survivable state keeps it
beside the stocked one.

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
RAM byte, mechanical field, serialized byte or the frame clock moved, and rolls
both regions back when any check fails. It is a standalone diagnostic, never a
search action and never a generated witness, so a replay must record and repeat
it. The intervention point is validated before any output file exists, and a run
that ends before the point is reported as the unintervened run.

`metroid-map-probe` replays a tape and prints the map cell and resources at each
action endpoint. Like `metroid-film` it defaults to `death_or_ending_v2`
and takes `--terminal-policy`, so a recorded tape is not stopped early
by a predicate it was never recorded under. A campaign report names the areas a run entered and counts the
map cells it observed; neither says which cells a route crossed, so neither can
say which neighbour of a reached cell was never opened.

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
highest present reading. It keeps the live Zebetite columns' remaining hits in
the cell below the map cell, and carries a lineage's highest reading across map
cells while a reading is present. Named progress
(`metroid-named-progress-v3`) records the Kraid and Ridley defeat flags, Mother
Brain's state, latched Tourian events and the destroyed Zebetite column, and
binds Mother Brain's room to her status byte. Route timestamps exclude genesis
setup. The replay probe reports action execution work and setup separately, and
probes and backend snapshot replay are outside the execution-work counter. The
campaign stream format is v4, the snapshot checkpoint format is v8, and the
result digest format is v5. The combined capacity score is a separate named score.

Retrospective replay, without submitting an existing solution to search:

```sh
cargo build --release --manifest-path workloads/nes/Cargo.toml --bin nes-progress
workloads/nes/target/release/nes-progress metroid CORE ROM searched-input.json > progress.json
```

The tool starts from ordinary new-game genesis, records named discoveries,
repeats the replay, and includes ROM/core/input hashes. In its replay report,
`first_seen.execution` counts tape actions, not historical search executions.
For Mega Man 2, `nes-progress mm2 CORE ROM searched-power-on-prefix.json wily4`
checks that the retained tape still reaches that stage through the current
runtime; it is a compatibility fixture, not a fresh search result.
