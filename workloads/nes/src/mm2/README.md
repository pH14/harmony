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
