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
`wily1_entered` through `wily6_entered`, their corresponding
`wilyN_boss_defeated` events, and `ending`. Robot Masters can occur in any order.
The Wily bosses are Mecha Dragon, Picopico-kun, Guts Tank, Boobeam Trap,
Wily Machine, and Alien, respectively. Castle boss defeats are confirmed by
the transition to the next stage, rather than a transient empty health meter.

Reporting also names `wilyN_room_R`, each `wily5_NAME_refight_defeated`, and
`wily5_machine_shell_broken`. These describe long legs without ranking room
numbers as progress. Dead observations cannot discover milestones. Each run's
hard parts are named by their own entry and exit events. Aggregate reports are
unions across search branches; only a witness tape establishes one trajectory.

`retained_diagnostics` carries the end-of-run census of the live archive.
`live_entries_by_screen` maps a screen to
`[entries, max health, max summed energy, selections]`, and
`live_entries_by_screen_row` maps `screen:16-pixel row from the top` to
`[entries, selections]`. The maxima alone hide a stall: a screen holding one
healthy endpoint and a screen holding a hundred thousand spent ones report the
same band, and a screen count cannot say which end of a shaft the archive sits
at or which end the selector draws. Both read only cached active endpoints, so
they are lower bounds where snapshots are missing.

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
The stream and checkpoint formats advance to v3 because decoded state and
encounter evidence changed; old search checkpoints must not resume under these
readings.

Completion verification also drives the whole-game target beside continuous raw
replay, checks mechanical agreement after every action, restores snapshots at
three points, and checks every required milestone at its recorded action index.
The six castle clears occur at actions 3603, 4010, 4177, 5074, 6456 and 6696;
the ending appears at 6697. Replay milestone stamps use tape action indices,
explicitly labelled in the output, rather than search executions.
