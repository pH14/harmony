<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Mega Man 2 workload

This package carries the native Mega Man 2 adapter onto the refactored campaign
contracts. The generic engine receives opaque keys, typed actions, observations,
and snapshots. This module owns every RAM address and game interpretation.

`Mm2Game::new_whole_game` and `mm2-campaign --whole-game` start at the
ordinary power-on stage-select menu, after the same fixed title-screen input
used by independent-stage cases. The search chooses the Robot Master order,
continues through deaths and individual boss defeats, and succeeds only at the
ending. Every frame after the menu origin is chosen by the search's generic
controller alphabet. Independent-stage cases still stop at their first clear.
No gameplay route, weapon choice, obstacle target, or boss weakness is injected.

The decoder defines RAM addresses in `target.rs`.
It observes stage/screen/room, position and posture, health, weapon/menu state,
weapon energy, boss/enemy damage, active platforms, and terminal events. It
corrects wrapped coordinates and transition states that caused false deaths in
the earlier experiments. Controller sampling covers nine directions times four
A/B combinations plus ordinary Start taps; Start is necessary to operate the
weapon menu. The v2 controller identifier corrects the prototype's stale
`no_start` label. No control is selected based on a named situation. That
vocabulary is the adapter's alphabet sampler and nothing else about drawing;
the searcher owns the suffix draw and the retained-input table.

The menu decoder reads bank `0x29 == 0x0d`; byte `0x04` is sprite scratch
and can equal the old menu marker during ordinary play. Enemy damage requires
an active object, a hit flag, and stable object identity (or a confirmed kill).
It persists across action endpoints and film replay; despawns do not earn damage.
A nonzero stale boss meter after Continue is not an encounter without an active
boss phase.

Wily 4 exposes the live barrier/trap mask and usable Crash shots (energy divided
by four). Wily 5 exposes the refight mask and active boss identity, normalizes
the stage byte borrowed during teleport only with a trusted Wily 5 origin, and
does not settle intermediate refight awards. The Wily Machine's shell break is
separate from damage; its second-form meter refill is not damage. Their observations also distinguish irreversible encounter states in the archive.

The v21 key keeps v20's 32-pixel places and 16-pixel holder identities, posture,
platforms, menu, weapon identity, boss damage and confirmed enemy damage. Health
and summed weapon energy remain preferences within a slot, not extra places.
The whole-game audit found two v20 collisions: different one-weapon inventories
shared a place, and a confirmed Wily 5 clear did not outrank its reset Wily 6
encounter. The key therefore adds the weapon capability mask, Boobeam target
mask, refight mask/identity and Machine form. Its progress order is ending,
Robot Master count, confirmed castle-clear count, refight count, then Machine
shell break. A castle clear outranks the encounter state that resets afterward.
No rooms-visited reward or resource-level cells are introduced. Summed energy
remains a resource-preference tradeoff, not dominance.

## Named progress

`workload_diagnostics.named_progress.first_seen` reports the first search
execution and route frame for each event. The required vocabulary is
`heat_defeated`, `air_defeated`, `wood_defeated`, `bubble_defeated`,
`quick_defeated`, `flash_defeated`, `metal_defeated`, `crash_defeated`,
each Robot Master's corresponding `NAME_entered`, `wily1_entered` through
`wily6_entered`, their corresponding
`wilyN_boss_defeated` events, and `ending`. Robot Masters can occur in any order.
The Wily bosses are Mecha Dragon, Picopico-kun, Guts Tank, Boobeam Trap,
Wily Machine, and Alien, respectively. Castle boss defeats are confirmed by
the transition to the next stage, rather than a transient empty health meter.

Reporting also names each Robot Master's `NAME_room_R`, plus `wilyN_room_R`, each `wily5_NAME_refight_defeated`, and
`wily5_machine_shell_broken`. These describe long legs without ranking room
numbers as progress. Dead observations cannot discover milestones. Each run's
hard parts are named by their own entry and exit events. Aggregate reports are
unions across search branches; only a witness tape establishes one trajectory.

`retained_diagnostics` carries an end-of-run census by the archive's actual
place key, including stage, screen, room, spatial position and capabilities.
Each row reports live entries, selections, maximum health, maximum lives and
independent maxima for all twelve weapon-energy bytes. Those maxima need not
belong to the same endpoint. Only cached active endpoints contribute, so the
census is a lower bound when snapshots are missing.

Whole-search checkpoints preserve named milestone stamps and campaign evidence.
`Mm2Game::with_root_input` replays a tape from the normal genesis before creating
an empty search archive. Its tape fingerprint is part of the workload identity,
so a checkpoint from another root cannot resume accidentally. Rooted output
tapes are relative to that root; prepend the root tape before using them as a
new power-on-relative root.

`mm2-film` replays a stage prefix and a searched tape to video, starting the
capture at stage genesis: the capture buffers are bounded, and a chain prefix
long enough to reach a castle stage would overflow them during construction.
`mm2-energy-probe` prints the twelve weapon-energy bytes at each action
endpoint, the last of which is the energy-tank count rather than a meter. The
decoded state also keeps the twelve individual bytes. These stage tools stop at
a boss clear; `mm2-replay` replays an entire power-on tape through the ending.

Use the common [local evaluation runner](../../../../benchmarks/search/README.md).

## Completion replay fixture

`fixtures/completion-input.json` is the unmodified 6,800-action completion tape
from commit `ea4d3116c` (#362). It is a replay oracle, not a root for new searches
or evidence of an autonomous power-on completion. No ROM or emulator is included.

With external `HARMONY_MM2_ROM` and `HARMONY_QUICKNES_CORE` paths, run:

```sh
cargo run --release --manifest-path workloads/nes/Cargo.toml --bin mm2-replay -- \
  workloads/nes/src/mm2/fixtures/completion-input.json --verify-completion
```

The replay queues the whole tape at power-on and runs continuously without
intermediate restores. Verification requires all 254,990 frames and the recorded
ending RAM marker (stage 5, boss phase 255, health 6, two lives, all weapons).
`--trace-output PATH` records decoded state and diagnostic RAM at action
boundaries; `--film-from INDEX --film-output PATH` captures a trailing film.

`recorded_tests.rs` pins the relevant RAM bytes from this tape at zero-based
action endpoints 232, 902–903, 3263, 4486, 4522, 5401 and 6437. The tests cover
sprite scratch versus menu bank, confirmed damage and replay tracking,
Boobeam targets/ammunition, stale Continue health, borrowed teleport stage,
and Machine refill. Unrelated bytes are zeroed in each focused decoder test.
The v4 stream and checkpoint readings changed decoded state, encounter evidence
and the required Robot Master entrance vocabulary; checkpoints from earlier
readings cannot resume under them. The current formats are v5 for the compressed
snapshot encoding described below. A fixed
`reporting_policy` identifier also guards whole-search checkpoint evidence.
The v2 reporting policy requires a stocked endpoint to satisfy its named
milestone as well as its entrance anchor. A scrolling transition can already
hold the next room byte without reaching its playable entrance; it cannot
supply that room's stock record. Checkpoints carrying v1 stock evidence are
rejected under v2.

Completion verification also drives the whole-game target beside continuous raw
replay, checks mechanical agreement after every action, restores snapshots at
three points, and checks every required milestone at its recorded action index.
The six castle clears occur at actions 3603, 4010, 4177, 5074, 6456 and 6696;
the ending appears at 6697. Replay milestone stamps use tape action indices,
explicitly labelled in the output, rather than search executions.

## Native snapshots

MM2 uses the shared native NES codec to store QuickNES state as a size-prefixed
LZ4 block, including in whole-search checkpoint snapshot records. Restore checks
the declared size against the loaded core before allocating, requires the exact
decompressed length, and then runs QuickNES's core-build, format and canonical
state checks. Imported handles are released after both successful and failed
replay. The snapshot keeps the game observations and completion evidence alongside
the compressed state.

The v5 stream and snapshot-checkpoint formats require
`snapshot_encoding=quicknes_lz4_size_prefixed_v1`. Whole-search checkpoint policy
validation rejects snapshots from the older raw-state encoding. Preserve old
checkpoint bundles with their original source and core; they cannot be resumed
as compressed snapshots.

Archive memory accounting charges the compressed state length. This can change
retention under a fixed memory budget, so results from raw-state and compressed
builds are separate cohorts. The append-only checkpoint store writes each new
snapshot ID once and keeps records needed by retained historical checkpoints.
LZ4 reduces the state payload; archive metadata and checkpoint indexes remain
uncompressed, and dropping an interval checkpoint does not compact the store.

## Whole-game evaluation tools

Build `mm2-eval`, `mm2-film` and `mm2-tape-probe` from this package. `mm2-eval`
accepts the shared native evaluator request and output-directory arguments.
Use it as `eval.py run --binary` with `benchmarks/search/mm2-whole-game.json`;
run one cell at a time (`--jobs 1 --cpus 8 --memory-capacity-mib 32700`). The
manifest uses six workers and a 16 GiB search budget. Keep original run bundles (including milestone tapes), checkpoint directories
and their shared snapshot store together, allow enough disk space with
`--disk-limit-gib`, and preserve the original memory budget on resume. The
manifest is a censored baseline; it does not require every seed to solve.

`whole_game: true` starts at the normal title-to-stage-select prefix. A
`root_input` is relative to that genesis, and a `resume` names a whole-search
checkpoint. Whole-game requests reject `stage`; stage requests retain the
independent-stage mode. The shared evaluator verifies both witness and
milestone tapes. `checkpoint_on_progress` saves named discoveries and new top
tiers; `checkpoint_every` additionally saves regular intervals.

`milestone-inputs/NAME.json` preserves the first discovery. Siblings ending in
`-health`, `-lives`, and `-energy-0` through `-energy-11` preserve resource records.
Energy indices follow RAM `$9c..$a7`; the last byte holds E-tanks. Records compare
only arrivals at the discovery's stage, screen, room, inventory, platforms and
boss state. Moving inside that room is permitted; later rooms or boss progress
cannot replace an entrance tape. Each quantity has its own tape: maxima across
tapes do not imply one state owns all resources. A first discovery may occur
inside an action, so its tape ends at that action's boundary; inspect the probe
before choosing a root. Record values and entrance anchors survive checkpoints.

With `HARMONY_MM2_ROM` and `HARMONY_QUICKNES_CORE` set:

```sh
mm2-tape-probe input.json
mm2-tape-probe input.json --screenshot endpoint.ppm
mm2-tape-probe rooted-input.json root.json
mm2-film whole-game empty-input.json input.json film.mp4
mm2-film whole-game root.json rooted-input.json rooted-film.mp4
```

An empty input is `{"actions":[]}`. Whole-game film continues through deaths and
Robot Master defeats, capturing from the selected root. The probe prints the
decoded endpoint, individual energy bytes, death state and ending flag after
exact replay. Its `named_progress` records discoveries on this one tape, with
one-based action indices; zero means a milestone already held at the root.
These are tape positions, not search tries. Use this order to name a route's
entry/exit legs; search-wide first-seen milestones can come from different
branches and do not establish a continuous route. Existing stage film and raw
RAM probe modes remain available.

Robot Master entrances and rooms require live health, a playable player state
(`$2c` in 2 through 10), and the gameplay bank
`$29 == $0e`; portrait selection and award screens reuse stage/room bytes in
bank `$0d`. Recorded RAM at completion-tape actions 22, 27, 406 and 1845 checks that
selection, live entry, a post-award menu and teleport-stage scratch remain distinct. The Heat room-2
qualification witness supplies a separate live traversal check. These markers
produce entrance tapes before the first boss, so early traversal stalls can
receive the same rooted and handoff analysis as castle stalls.

Stock comparisons use live boss damage, not the previous boss phase left over
during a castle award transition. This allows the ordinary full-health arrival
to improve an entrance recorded during the transition without admitting a
state that has already damaged the next boss. Zero-health endpoints cannot
improve stock records.

An `entered` milestone means the first playable arrival, after the award/map
transition and health refill, rather than the first changed stage byte. This
keeps its stocked tapes in the actual starting room: for example, Wily 2
transitions through room 0 before play begins in room 22. Boss-clear milestones
still record the stage transition. Already-completed milestones can be inferred
at a rooted start, where their tape counter is zero.

The probe's optional `--screenshot` writes the final captured frame as a PPM
image without launching a video encoder. Capture buffers are drained after
each action. This permits single-core visual inspection alongside a bounded
search. An empty input has no new frame and cannot produce a screenshot.
