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

The opt-in `metroid_terminal: death_or_bcd_underflow_or_ending_v3` evaluation
policy also marks decoded health >=8000 as terminal. The damage routine stores
a BCD subtraction before testing borrow and clearing lethal damage; a frame
boundary can expose this intermediate value. A verified development endpoint
reported 9800 and died under every one-frame controller mask. Raw health remains
unchanged for replay inspection. Historical v2 is still the default; stream
headers distinguish the policies and reject a mismatched replay context.
This correction is independent of the optional archive-key refinement.

`archive.rs` records the experimental adapter policy explicitly. It pools
16-pixel positions through 32-pixel cells, 128-pixel regions, map cells, and
inventory counts. Posture and door-transition state distinguish possible
continuations. Health and missile stock are same-slot preference, not extra
spatial slots. Item and tank counts describe discovered capabilities; no
particular item, room, door target, or route is supplied. Coverage and pickup
counters are reporting evidence across explored branches, not proof that one
trajectory achieved their union. The inherited count representation and
lexicographic resource preference are policy tradeoffs, not true capability
or resource dominance.

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

Use the common [local evaluation runner](../../../../benchmarks/search/README.md)
for paired search comparisons and full small-campaign replay. `metroid-campaign`
also exposes the native experiment command. The source lineage is documented in
[the synthesis record](../../../../benchmarks/search/SYNTHESIS.md).

## Named milestone evaluation

`workload_diagnostics.named_progress` reports Morph Ball, Bombs, Long Beam,
High Jump, Screw Attack, Varia Suit, Wave Beam, and Ice Beam independently;
Brinstar, Norfair, Kraid's area, Ridley's area, and Tourian independently; and
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
and the deterministic endpoint. Its main champion/victory witness separately
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
reports physical work and setup separately. Earlier v1 timestamps in the 007
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

The opt-in `retention_audit` evaluation flag records at most 16 competing input
pairs in each of five diagnostic categories: different equipment, different
capacity, resource tradeoffs, equal preference, and ordered resources. A separate
sampling hash leaves the campaign RNG untouched. Incumbents with evicted cached
snapshots are counted but excluded, so samples are not a census of every state.
The declared 2.5 MiB action-payload bound plus metadata/reconstruction and output
buffers are diagnostic overhead reflected in RSS and I/O, outside logical archive
memory. `metroid-retention-probe` reconstructs each pair and applies the same
sampled suffixes to both sides. It reports physical probe/prefix frames, gains,
survival, and living map exits separately. These diagnostic starts never count
as fresh validation; differing endpoints alone are not useful-future evidence.

The probe accepts `CORE ROM AUDIT OUT TRIALS ACTIONS [TERMINAL_POLICY [SEED]]`.
Omitting the final arguments keeps the historical terminal predicate and suffix
seed. Its v2 report records the terminal identity, numeric seed, and exact suffix
hash. Use terminal v3 for audits from corrected campaigns; living-map checks
respect that predicate. A separately registered seed permits new suffix probes
without silently reusing the original development suffix bank.

The opt-in Cargo feature `metroid-refined-archive` builds a separate experimental
v9 key policy. It uses 8-pixel retention positions and raw pose, while preserving
the existing 32-pixel selection cells, 128-pixel regions, map groups, progress
ordering, resource preference and one representative per retention slot. The
default build retains v8 semantics and serialization. Streams record distinct
key-policy identifiers and reject replay under the other build's policy.
The key's serialized field layout is unchanged; the recorded policy identifies
the position/pose interpretation. Archive imports already re-derive keys from
reconstructed states rather than copying the old key identity.

This is an abstraction experiment, not a validated improvement. The frozen
numeric continuation probes predict that it separates 11 of 15 sampled pairs
with distinguishing local exits or survival outcomes. More slots consume the
same archive byte budget, and retained alternatives may still receive too little
work. Actual-ROM replay and matched fresh search must qualify it before use.
See the [research ledger](../../../../benchmarks/search/retention-theory/README.md).

## Standalone boss-memory diagnostics

`metroid-boss-probe CORE ROM INPUT.json OUT` reads raw loader and enemy-slot
bytes while replaying an existing searched tape. It compares ordinary chords
with two independent one-frame replays, requiring equal final emulator bytes
and decoded state plus identical frame-trace hashes. Inputs are bounded at
8,192 actions/250k route frames; private trace output is capped at32MiB.
No probe value changes search observations, snapshots, retention or termination.
The standalone probe uses corrected terminal v3 and stops on a terminal tape.

The raw addresses come from the pinned
[disassembly](https://github.com/nmikstas/metroid-disassembly/tree/4270d57f9468daebdeea485686e31e26218a780c):
`Metroid_Defines.asm` and Bank07 loader, slot, HP and hit routines. A special
byte can change during combat or remain in an inactive slot. The report keeps
loader, slot status, tag and HP separate; it does not infer damage totals,
encounter identity or defeat from them. An empty existing-route trace is only
a negative control, not evidence about every route in its source campaign.

## Retrospective motion descriptors

`metroid-kinematics-probe CORE ROM AUDIT.json OUT` reconstructs at most sixteen
recorded replacement pairs and reads facing, signed-speed bytes and four motion
accumulators from the same pinned disassembly. Each endpoint is replayed twice,
must match its recorded mechanical state, and must retain identical snapshot
bytes before and after the read. The complete run is capped at 2M physical
frames; each input is bounded at 8,192 actions and 250k route frames.
`pairs.jsonl` retains completed per-pair evidence if a later check fails.

The probe uses terminal v3 and runs no suffix trials or fresh search. No field
is added to search observations, snapshots, keys or policies. Comparing these
descriptors with existing continuation outcomes can refute a proposed grouping;
it cannot establish that motion alone causes the difference or that keeping
its groups improves discovery. The coarse descriptor and design gate are
registered in the [P04 ledger](../../../../benchmarks/search/retention-theory/README.md).
