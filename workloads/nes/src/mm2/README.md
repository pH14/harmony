<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Mega Man 2 workload

`mm2-campaign --root-input INPUT.json` runs a diagnostic from a recorded
stage-relative input after the usual stage setup and optional continuous
`--prefix-input`. Every root action is replayed through the target; terminal
roots are rejected. The root becomes the reset point and participates in the
workload identity. Generated victory prefixes include its actions. Search
outputs are relative to that root, so concatenate `root-input.json` before
the searched suffix when using `mm2-film` with the original stage prefix.
Rooted progress is a diagnostic and must be replayed from power-on before it
counts as continuous game progress.

`mm2-campaign --whole-game` starts at the recorded power-on stage-selection
prelude. The search supplies all stage choices, menu inputs, deaths/retries,
Continue decisions, and award/ending waits. Only the final ending is the
objective. No transition frames are supplied by the adapter after genesis.
The default whole-game tape budget is 32,768 actions. This mode rejects explicit stages, prefixes, manual roots, coordinate-consistency
pruning, and selector/retention/mixture overrides. Archive warm starts retain
normal identity checks and must originate from this same whole-game policy.
The CLI releases validated raw archive bytes after decoding and hashing the
report, before constructing checkpoint snapshots, to reduce import peak memory.

`--selector`, `--retention`, and `--mixture` are diagnostic controls for isolated
stage experiments, not whole-game policy knobs. The ordinary fixed selector is
`hierarchy_uniform_128_energy_frontier_cheapest:3,6,12,2,16`; retention is
`unprobed`. Their generic implementations remain in the searcher.

This package carries the native Mega Man 2 adapter onto the refactored campaign
contracts. The generic engine receives opaque keys, typed actions, observations,
and snapshots. This module owns every RAM address and game interpretation.

Death-animation hysteresis is part of each observation and snapshot. Its
consecutive-frame count persists across action boundaries; splitting a hold
into shorter inputs must not reset the death timer. Stream and snapshot
stream format is version 9 and the snapshot format remains version 8. The stream
version includes the whole-game lifecycle, raw resource observations, and the
source-backed guard that excludes stale dead-player boss phases from actionable
partial-damage progress.

Registered cases start from power-on menus selecting one of the eight ordinary
Robot Master stages. A stage clear is reported as an independent stage result;
these runs do not constitute a continuous whole-game solution. The adapter also
preserves the earlier probe/campaign tools for examining recorded discoveries.
No gameplay route, weapon choice, obstacle target, or boss weakness is injected.

The decoder defines its RAM addresses in `target.rs`; interpretation is documented here.
It observes stage/screen/room, position and posture, health, weapon/menu state,
weapon energy, boss/enemy damage, active platforms, and terminal events. It
corrects wrapped coordinates and transition states that caused false deaths in
the earlier experiments. Controller sampling covers nine directions times four
A/B combinations plus ordinary Start taps; Start is necessary to operate the
weapon menu. The v2 controller identifier corrects the prototype's stale
`no_start` label. No control is selected based on a named situation. That
vocabulary is the adapter's alphabet sampler and nothing else about drawing;
the searcher owns the suffix draw and the retained-input table.

The `mm2-campaign` CLI accepts the opt-in `--coherent-world` diagnostic. It
marks a live, same-stage observation terminal when the player screen byte and
camera room byte differ by more than one while the raw scroll-direction byte
(`$37`) is idle. The option is experimental, defaults off, and is included in
the `Mm2Game` identity so streams and snapshots cannot mix the two policies.

The legacy weapon-menu observation uses the current bank byte at `$29` (`$0d`),
followed by the existing cursor/page bytes. `$04` is sprite/menu scratch,
not a game mode: a recorded open/close probe shows it becoming three in
game-world frames after the menu closes. The bank-based decoder is tied to
the pinned ROM and frame-boundary observation protocol; new cores or ROMs
must recheck opening, closing, and gameplay traces. The same bank and scratch
bytes also occur in Game Over and password code; they are not a universal scene
classifier. Raw bank/cursor/page observations remain available for diagnosis.
The key keeps only candidate menu selections within the source-defined page
and row bounds; all other bank-`$0d` scratch values share one unknown/transition
value. This avoids treating thousands of reused scratch combinations as new
controllable states. A valid tuple still does not prove a particular scene.
`$f7` is the PPU-control shadow, not a game-mode byte. Whole-game execution
uses no `$f7` scene predicate or health-based terminal exclusion.

Boss-entry milestones require an active encounter or a confirmed defeat.
The boss HP byte can remain 28 after Game Over/Continue even though the boss
phase has reset to zero at the stage start. The current stream records the
corrected milestone semantics; nonzero boss HP alone is not entry evidence.
Raw boss phase and HP bytes remain in every decoded state. Derived partial boss
damage and active-fight status require a live player status and nonzero player
health, so stale phase/HP left by death and menu code cannot become actionable
progress. A defeated phase remains a source-backed clear marker, including a
same-frame player death.

The v27 key uses 16-pixel retention slots pooled into 32-pixel cells,
128-pixel regions, screens, and stages. Exact acquired-weapon masks distinguish
capability identities at every depth; their popcounts determine progress.
Different sets with equal counts have equal progress and remain separate.
All twelve resource bytes are retained uniformly at the finest depth, including
the energy-tank count in the last byte, and pooled above it. This removes the
old Crash-only, encounter-gated ammunition identity. It is a resource-diversity
hypothesis, not a dominance claim; summed energy remains the representative
preference tradeoff. No weapon is preferred for any obstacle.

The experimental route context preserves the finest retention group's physical
fields, selected weapon, menu, encounter state, and all twelve weapon/item
resource bytes, while omitting acquired-weapon identity and its count. It identifies candidate
matches for the generic searcher's route-reuse experiment; it does not promise
that a recorded route will work from a different state. Full capability
identity remains in every retention group. The searcher chooses and executes
observed tails and evaluates their actual arrivals. The default campaign
policy does not enable this experiment.

Weapon/menu observations distinguish local endpoints but are pooled at coarser
levels. Stage, room and screen bytes identify locations; progress uses boss
clears and current boss damage, not coordinate ordering. Boss damage is zero
until the boss loads its health, and full once the phase reports defeat.
A Wily boss grants no weapon, so encounter completion also observes the defeated
phase. The documented Wily4 object mask still distinguishes live trap/barrier
objects without a target priority or resource prescription. The searcher owns
selection, retention, rollout length, mutation, and continuation reuse.

Whole-game `castle_clears` records confirmed castle boss-clear transitions:
source stage 8 through 13 advances by one after boss phase `$ff`. A completion
bitmask prevents repeated clears from increasing the count, survives Continue,
and resets with new-game progress. It is snapshot state, not a coordinate rank
or route reward. Progress compares this count before the current refight and
boss-damage fields, so entering the next castle stage remains an improvement
after encounter-local fields reset.

The Wily5 extension records the refight completion mask from `$BC` at stage 12
and keeps it at every retention depth; it is zero outside Wily5. The eight mask
bits correspond in order to Heat, Air, Wood, Bubble, Quick, Flash, Metal, and
Crash. While a Wily5 boss is actively
fighting (phase `0x02` through `0xfd`), `refight_boss` records `$B3` in local
retention groups 0 through 2. The hub, phase-1 boss intro, defeated phase, and
other stages use sentinel `0xff`, preventing a stale boss ID from splitting
hub states. Progress compares the completed-refight count before boss damage,
so raw mask values with the same bit count are equal in progress ordering.
For Wily Machine specifically (stage12, bossID12), AI states4 through0xfd
identify the broken-shell form. This bit is retained at every depth and ranks
after refight count but before damage. AI state4 refills its health bar from1
to28; it contributes zero damage. Treating that refill as damage previously
ranked its earliest frame above the actual second fight. The field is gated
out for other bosses, other stages, and defeat. The AI state is not a general
boss-form enum; these meanings were verified on a continuous transition tape.
An intermediate Wily5 phase-`0xfe` observation is a reached encounter but a
stage clear only when `$BC == 0xff`; target terminal stopping remains based on
the existing stage/weapon objective.
During Wily5 teleport setup the ROM briefly borrows a Robot Master stage ID in
the raw stage byte while the player status is `0x0b`. The target decoder treats
that specific borrow as semantic stage 12 only when its trusted genesis is
Wily5, so it cannot become a false death or a separate stage class; refight
fields remain stage-gated and preserve `$BC` through the borrow.
Enemy damage requires a health decrease on an active enemy with a confirmed
hit flag, preserving its object and spawn identity or its killed-object
transition. Twenty HP is a real initial health value, not an idle sentinel.
The accumulator survives action endpoints, reset and snapshot restore. A
recorded Leaf Shield hit on Sniper Armor changes its health from 20 to 6 while
the player remains alive; the per-hit reward is capped at four. These semantics
follow the game's [damage handler](https://github.com/lsmmega/mm2/blob/master/home/weapons_enemies_damage.asm)
and are checked against the recorded frame trace.
The v17 prototype is preserved in the preceding commit and benchmark build;
replay rejects a different recorded policy instead of silently reinterpreting it.

`retained_diagnostics` carries the end-of-run census of the live archive.
`live_entries_by_screen` maps a screen to
`[entries, max health, max summed energy, selections]`, and
`live_entries_by_screen_row` maps `screen:16-pixel row from the top` to
`[entries, selections]`. The maxima alone hide a stall: a screen holding one
healthy endpoint and a screen holding a hundred thousand spent ones report the
same band, and a screen count cannot say which end of a shaft the archive sits
at or which end the selector draws. Both read only cached active endpoints, so
they are lower bounds where snapshots are missing.

The published champion witness uses a report-only ordering of factual progress:
defeated-boss count, castle clears, refight count, Wily Machine shell state,
live boss and enemy damage, and active encounter status precede raw stage and
location labels. A stale boss RAM value in a death or menu state therefore
cannot outrank a real active encounter. This ordering does not alter archive
keys or selector behavior; `first_boss` remains the independent first-encounter
witness.

`mm2-film` replays a stage prefix and a searched tape to video, starting the
capture at stage genesis: the capture buffers are bounded, and a chain prefix
long enough to reach a castle stage would overflow them during construction.
Campaign film output streams RGB frames through a single-thread FFmpeg encoder
and then muxes the recorded PCM audio; temporary storage holds compressed video
and PCM rather than the complete raw RGB frame sequence.
`mm2-energy-probe` prints the twelve weapon-energy bytes at each action
endpoint, the last of which is the energy-tank count rather than a meter. The
decoded state now also retains the individual bytes. The isolated-stage tools
stop once a boss is down; whole-game execution remains runnable through clears.

mm2-replay is the raw power-on replay path. It loads the external ROM and
QuickNES core directly, replays every action in an input archive, and never
adds stage setup or stops at a death or boss clear. It prints the ROM, core, and
input hashes. It stages the full controller tape once at power-on and advances
to action boundaries without intermediate snapshot restores, independently
checking the searcher's use of restore. The report also includes the
number of recorded and applied actions, the raw emulator frame
count, and the decoded endpoint. To capture a film beginning at a zero-based
action index while still replaying the complete tape, pass
--film-from ACTION_INDEX --film-output OUTPUT.mp4. The film includes that
action and all following actions; the replay report still covers the entire
input archive.
Pass --trace-output TRACE.jsonl to emit one JSON object per action after the
film start index (or action zero when no film is requested). Each trace object
includes the decoded state, action, raw frame count, object IDs, flags, X/Y
tables, object temporary state, boss RAM0xb0..0xbf, difficulty, the
0x6c0..0x6df health region, and the 12 weapon-energy bytes. Use
--trace-from INDEX to choose a trace start without changing film capture.

Use the common [local evaluation runner](../../../../benchmarks/search/README.md).
The source lineage and discarded search claims are listed in the
[synthesis record](../../../../benchmarks/search/SYNTHESIS.md).

`mm2-campaign --resume-archive ARCHIVE.json` imports the retained archive from
an earlier run as a warm start. Add `--resume-snapshots SNAPSHOTS.bin` when
the matching retained snapshots are available; the checkpoint format and file
hash are recorded in the new stream origin. The import rebuilds the archive
under the current ROM, core, root, selector, retention, and workload identity
settings. It is an archive warm start, not an exact continuation of the prior
campaign's random schedule or worker state.

Every archive output has a sibling `archive.manifest.json`. The manifest binds
the exact archive bytes, the optional snapshot bytes, the ROM/core workload
identity, and the resolved policy map. Archive warm starts require this sibling
and reject missing, changed, or cross-workload artifacts before importing them;
qualified runs always include a snapshot hash, while marketing runs include one
when `--save-checkpoint` is used.

Marketing soaks may write their returned retained snapshots with
`--save-checkpoint`, which creates `snapshots.bin` beside `archive.json`.
This flag requires `--marketing-soak`; qualified campaigns always write their
checkpoint as part of their existing output. `--resume-snapshots` requires
`--resume-archive`, and both modes preserve the existing root-input identity in
the new campaign stream.

`mm2-snapshot-inspect SNAPSHOTS.bin` emits one JSON record per retained
snapshot, including its archive id, decoded mechanical state, frame count, and
the serialized death and dying-run status. It emits only nonfailed,
nonterminal, non-dying snapshots by default; pass `--all` to include filtered
records. The filter uses the recorded observation status rather than treating
health alone as proof of survival. Passing this status filter does not establish
future survival; replay and film are still required. Post-menu transients can
also use the dying state byte, so `--all` is useful when auditing exclusions.

`mm2-branches STAGE PREFIX.json ROOT.json BANK.json OUTPUT.json` evaluates
an explicit JSON array of `Mm2Input` suffixes from one restored root snapshot.
It uses `HARMONY_MM2_ROM` and `HARMONY_QUICKNES_CORE`, records input hashes,
per-action observations, actual work and terminal status, and stops a branch
at death or stage completion. Include a tail in each suffix when testing
landing or survival. This is a controlled local probe, not a campaign or an
independent power-on verification; replay a successful composed tape separately.
