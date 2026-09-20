<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Metroid workload

This package ports the native Metroid experiments to the refactored campaign
contracts. Execution, controller interpretation, RAM decoding, setup, and
outcome evidence stay in `nes-workload`; archive search and route reuse stay in
`searcher`. ROMs and core binaries are private local inputs.

The registered workload starts a new game through ordinary power-on menus.
Equipment, tanks, boss defeat flags, and the ending flag come from cartridge
work RAM. The decoder maps Samus's screen using name-table membership and scroll
direction, so camera coordinates do not masquerade as player coordinates. All
source addresses and meanings are documented beside their constants in
`target.rs`. Zero health is death; the ending flag is victory. The terminal
identifier is `death_or_ending_v2`, correcting the prototype's stale
`death_only_v1` label without changing that prototype's predicate.

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
was not recorded under. The historical
`death_or_ending_v2` predicate is still selectable, through the evaluator's
`metroid_terminal` request field and `MetroidGame::with_terminal_policy`. Stream
headers carry the chosen identifier and reject a mismatched replay context.
Recorded tapes made before this policy existed replay under the historical
predicate: `nes-progress` names it, and `metroid-film` defaults to it.

`archive.rs` records the experimental adapter policy explicitly. It pools
16-pixel positions through 32-pixel cells, 128-pixel regions, map cells, and
inventory counts. Posture and door-transition state distinguish possible
continuations. Health and missile stock are same-slot preference, not extra
spatial slots, and the two preferences order that pair against each other in
opposite ways. The key's tank count subtracts the 75 missiles each boss kill
awards, so a kill does not relabel every map cell the killer reaches as holding
fifteen more tanks than the cells beside it; the kill still counts through the
item term. The Brinstar statue room rewrites both boss bytes to `0x82` when
Samus approaches the statues: bit 7 still counts as the defeat, and each raised
statue adds one more item. The raise opens the passage beneath the statues
without changing position, health or stock, so without that term the opened
state is a same-key duplicate of the entry that triggered it and is never
admitted. Item and tank counts describe discovered capabilities; no
particular item, room, door target, or route is supplied. Coverage and pickup
counters are reporting evidence across explored branches, not proof that one
trajectory achieved their union. The inherited count representation and
lexicographic resource preference are policy tradeoffs, not true capability
or resource dominance.

The adapter supplies its controller vocabulary as the alphabet sampler and
nothing else about drawing; the searcher owns the suffix draw and the
retained-input table.

`progress_cmp` compares the item count then the boss damage, so that is the
whole progress relation the selector reads. Tanks are capacity, so they live in
the preferences, which decide which states keep a location's slots. Field
declaration order no longer ranks anything: two places with equal items and
equal boss damage are peers whatever their area byte, map row or column.

Boss damage is how far a lineage has worn down the mini boss sharing its room.
In Tourian the same coordinate reads Mother Brain: while her status byte at
`$98` says she is in the room or has just been hit, boss health is 32 minus
her hit count at `$99`, the count she dies at; outside Tourian and in her
other states the mini-boss reading applies.
The game keeps six enemy slots at `$0400`, sixteen bytes apart, with the current
hit points at offset `$0b` and a mini-boss mark in bit 6 of offset `$0f`; `$ff`
hit points mean the slot holds nothing that can be hurt. The key carries the
remaining hit points and the highest present reading over the execution's
frames, and the lineage carries the highest reading it has seen, so the damage
is the difference between that highest and the remaining hit points in buckets
of four. A reading of nothing keeps the parent's damage, because a hit flashes
the slot empty for a few frames. A child that lands in a different map cell
from its parent starts from zero, because the boss regains full health when the
room is re-entered. Without the coordinate a state that has landed ten hits on
Kraid shares a cell with one standing in the doorway, and no ordering can
prefer the first.

The key declares two preferences. Both lead with items then tanks; the first
then ranks missiles before health and the second health before missiles. A
location keeps the best state under each, so at most two, and one state holds
both places when it leads on both. The two disagree only on a resource trade:
ten missiles at twenty health takes the first, five missiles at two hundred
health takes the second, and a route that needs the survivable state no longer
loses it to the stocked one.

The legacy primary progress watermark records equipment bit count **plus boss
defeats**, and missile capacity.
The `milestones.tanks` field combines missile capacity divided by five
and energy tanks (boss capacity bonuses also inflate that legacy field), so it
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
Endpoints holding different equipment in one cell sit in different archive
classes and are drawn separately, so a cell's own totals cannot say whether the
endpoints that can open the next door are the ones the selector goes back to.
Both read only cached active endpoints, so they are lower bounds where snapshots
are missing.

Use the common [local evaluation runner](../../../../benchmarks/search/README.md)
for paired search comparisons and full small-campaign replay. `metroid-campaign`
also exposes the native experiment command. The source lineage is documented in
[the synthesis record](../../../../benchmarks/search/SYNTHESIS.md).

`metroid-film` replays a recorded tape to video. A tape carries no policy
header and the recorded tapes predate the BCD-borrow predicate, so it defaults
to the historical one and takes `--terminal-policy` to name another.
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
action endpoint. Like `metroid-film` it defaults to the historical terminal
predicate and takes `--terminal-policy`, so a recorded tape is not stopped early
by a predicate it was never recorded under. A campaign report names the areas a run entered and counts the
map cells it observed; neither says which cells a route crossed, so neither can
say which neighbour of a reached cell was never opened.

## Named milestone evaluation

`workload_diagnostics.named_progress` reports Morph Ball, Bombs, Long Beam,
High Jump, Screw Attack, Varia Suit, Wave Beam, and Ice Beam independently;
Brinstar, Norfair, Kraid's area, Ridley's area, and Tourian independently;
the Kraid door cell, the Kraid, Ridley and Mother Brain rooms (a boss health
reading above zero in the boss's area), and Tourian's row-7 corridor at
columns 5 and 8, its bottom row (row 11 or below) and that row at columns
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
two more tapes per milestone, `NAME-energy.json` and `NAME-missiles.json`: the
living action endpoint that satisfied the milestone with the most health
(missiles breaking ties) and the one with the most missiles (health breaking
ties), rewritten whenever a later endpoint strictly beats the held one. A first
arrival is usually drained, so a search rooted at it starts short of what the
archive already holds at that place; these tapes make the best-stocked arrival
available as a root. They are not verified by the runner. Its main champion/victory witness separately
reports one trajectory's named progress. Do not call a union over search branches
one successful playthrough. Discovery-tape verification is charged to the
verification phase; the bounded export cost during discovery is part of search.
A first named discovery reconstructs its tape regardless of output configuration,
so publication options cannot alter deterministic report counters.
The live observer includes only observations admitted in this run, not a restored
archive's complete history. Missing fields in older reports mean **unavailable**,
not zero. A retained champion replay cannot establish everything an older search
explored or prove that no other branch defeated a boss.

The names and boss flags follow
[`Metroid_Defines.asm`](https://github.com/nmikstas/metroid-disassembly/blob/4270d57f9468daebdeea485686e31e26218a780c/Source_Files/Metroid_Defines.asm),
with the defeat write in `Bank07.asm` at `LDD75`: `(InArea & 0x0f) >> 1`
stores 1 at $687B for Kraid and 2 at $687C for Ridley. The previous decoder
incorrectly tested bit 0 for both bosses. Correcting the count is versioned as
key policy v8; named boss observation bytes require stream/checkpoint/result
digest v4 (v2 introduced named Kraid/Ridley flags; v3 added Mother Brain state;
v4 latches transient Tourian events). Named-progress v2 and replay-probe v2 also
correct origin/retrospective route timestamps to exclude genesis setup; the probe
reports action execution work and setup separately; probes and backend snapshot
replay are outside the execution-work counter. Earlier v1 timestamps in the 007
audit are superseded by the corrected audit, not silently rewritten.
Existing v7 results and the v2 reporting measurements remain immutable. The unrelated legacy combined
capacity score remains explicit rather than silently redefining past policies.

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
