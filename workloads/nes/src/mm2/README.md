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

`--selector` accepts the existing recorded selector-policy identifier. The
default remains `hierarchy_uniform_128_energy_frontier_cheapest:3,6,12,2,16`.
For a retry-limit ablation, change only its first threshold, for example to
64; the engine's hard exhaustion ceiling still applies.
`--retention` selects `unprobed` (the default) or the existing
`probe_at_admission` survival check. Compare its extra emulated work as well
as retained progress.

This package carries the native Mega Man 2 adapter onto the refactored campaign
contracts. The generic engine receives opaque keys, typed actions, observations,
and snapshots. This module owns every RAM address and game interpretation.

Death-animation hysteresis is part of each observation and snapshot. Its
consecutive-frame count persists across action boundaries; splitting a hold
into shorter inputs must not reset the death timer. Stream and snapshot
formats are version 2 for this serialized state and terminal-policy change.

Registered cases start from power-on menus selecting one of the eight ordinary
Robot Master stages. A stage clear is reported as an independent stage result;
these runs do not constitute a continuous whole-game solution. The adapter also
preserves the earlier probe/campaign tools for examining recorded discoveries.
No gameplay route, weapon choice, obstacle target, or boss weakness is injected.

The decoder documents RAM addresses alongside their definitions in `target.rs`.
It observes stage/screen/room, position and posture, health, weapon/menu state,
weapon energy, boss/enemy damage, active platforms, and terminal events. It
corrects wrapped coordinates and transition states that caused false deaths in
the earlier experiments. Controller sampling covers nine directions times four
A/B combinations plus ordinary Start taps; Start is necessary to operate the
weapon menu. The v2 controller identifier corrects the prototype's stale
`no_start` label. No control is selected based on a named situation. That
vocabulary is the adapter's alphabet sampler and nothing else about drawing;
the searcher owns the suffix draw and the retained-input table.

The v19 key uses 16-pixel retention slots pooled into 32-pixel cells,
128-pixel regions, screens, and stages. Weapon/menu rows distinguish local
endpoints but are pooled at coarser levels. Health and energy prefer
representatives without multiplying spatial slots. It removes the prototype's
rooms-visited lineage reward: returning to the same endpoint has the same key,
regardless of the number of rooms visited. Stage, room and screen bytes identify
locations; the progress relation uses boss clears and current boss damage.
Boss damage is zero until the boss loads its health, so a Wily boss that spawns
for its approach with an empty meter reads as no damage rather than a full bar,
and it is full once the phase byte reports the boss dead. A Wily boss grants no
weapon, so a cleared boss is a granted weapon or that same defeated phase byte.
The generic progress-aware selector consumes that relation. Historical frontier
selectors retain their original identity ordering for controlled baselines.
Summed energy remains a documented resource-preference tradeoff, not dominance.
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

`mm2-film` replays a stage prefix and a searched tape to video, starting the
capture at stage genesis: the capture buffers are bounded, and a chain prefix
long enough to reach a castle stage would overflow them during construction.
`mm2-energy-probe` prints the twelve weapon-energy bytes at each action
endpoint, the last of which is the energy-tank count rather than a meter. The
decoded state keeps only their sum, which cannot say whether the one weapon a
wall needs still has ammunition. Both stop once a boss is down, because the
target refuses actions from there.

mm2-replay is the raw power-on replay path. It loads the external ROM and
QuickNES core directly, replays every action in an input archive, and never
adds stage setup or stops at a death or boss clear. It prints the ROM, core, and
input hashes, the number of recorded and applied actions, the raw emulator frame
count, and the decoded endpoint. To capture a film beginning at a zero-based
action index while still replaying the complete tape, pass
--film-from ACTION_INDEX --film-output OUTPUT.mp4. The film includes that
action and all following actions; the replay report still covers the entire
input archive.
Pass --trace-output TRACE.jsonl to emit one JSON object per action after the
film start index (or action zero when no film is requested). Each trace object
includes the decoded state, action, raw frame count, object IDs, flags, X/Y
tables, the 0x6c0..0x6df health region, and the 12 weapon-energy bytes. Use
--trace-from INDEX to choose a trace start without changing film capture.

Use the common [local evaluation runner](../../../../benchmarks/search/README.md).
The source lineage and discarded search claims are listed in the
[synthesis record](../../../../benchmarks/search/SYNTHESIS.md).
