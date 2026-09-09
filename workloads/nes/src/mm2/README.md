<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Mega Man 2 workload

This package carries the native Mega Man 2 adapter onto the refactored campaign
contracts. The generic engine receives opaque keys, typed actions, observations,
and snapshots. This module owns every RAM address and game interpretation.

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
`no_start` label. No control is selected based on a named situation.

The v18 key uses 16-pixel retention slots pooled into 32-pixel cells,
128-pixel regions, screens, and stages. Weapon/menu rows distinguish local
endpoints but are pooled at coarser levels. Health and energy prefer
representatives without multiplying spatial slots. It removes the prototype's
rooms-visited lineage reward: returning to the same endpoint has the same key,
regardless of the number of rooms visited. Stage, room and screen bytes identify
locations; the progress relation uses boss clears and current boss damage.
The generic progress-aware selector consumes that relation. Historical frontier
selectors retain their original identity ordering for controlled baselines.
Summed energy remains a documented resource-preference tradeoff, not dominance.
The v17 prototype is preserved in the preceding commit and benchmark build;
replay rejects a different recorded policy instead of silently reinterpreting it.

Use the common [local evaluation runner](../../../../benchmarks/search/README.md).
The source lineage and discarded search claims are listed in the
[synthesis record](../../../../benchmarks/search/SYNTHESIS.md).

`nes-eval` also accepts an explicit `mm2_chain` stage origin and an optional
power-on `prefix_input` with a required SHA-256. On a searched victory it emits
`full-victory-input.json` and `next-prefix.json`, using the existing adapter-owned
award/menu transition. `chain-cost.json` counts repeated target construction
(including every prefix replay) and the transition helper's physical frames.
Witness suffix frames are reported independently. Partial setup failures have
incomplete cost accounting and cannot be scored as successful searches.
The research driver in `benchmarks/search/alternative-futures/mm2_chain.py`
accepts no gameplay prefix; it carries only prior searched victories from its
own new output directory under the recovered fixed historical order. This is
chained qualification, distinct from unrestricted whole-game search.

Physical chain exports expand the adapter's automatic award-idle frames into
explicit zero-button holds. Concatenating the originally sampled holds alone
omits that executed work and can fail the next-stage replay. `physical_input`
replays and expands those holds without changing campaign action semantics.

`mm2-metal-export CORE ROM VICTORY.json OUT` qualifies the existing chain
export path from a searched Metal victory without repeating search. It uses
the adapter's physical-input expansion and ordinary award/menu transition,
records their frame costs, and writes a private next-stage prefix. A separate
twice-replayed next-stage bridge remains required. It does not make a supplied
victory into a fresh chain result.

A replayed stage-award witness can report both `victory: true` and
`dead: true` at its final endpoint. Those raw flags do not alone establish
continued gameplay. Keep them in the evidence, then qualify chainability by
replaying the ordinary award/menu transition and next-stage entry with the
awarded inventory retained. The R03b Metal qualification records this case
for both policies, with twice-replayed Heat entry at full health.
