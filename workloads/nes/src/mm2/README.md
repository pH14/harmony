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

The inherited v17 key is an explicit experimental policy: 16-pixel retention
slots pool into 32-pixel cells, 128-pixel regions, screens, and stages. Weapon/menu
rows distinguish local endpoints but are pooled at coarser levels. Health and
energy prefer representatives without multiplying spatial slots. The inherited
rooms-visited component, numeric ordering, and summed energy are potential
biases that require separate adapter ablations; opaque typing does not make
those choices neutral. Keep adapter identity fixed during engine comparisons.

Use the common [local evaluation runner](../../../../benchmarks/search/README.md).
The source lineage and discarded search claims are listed in the
[synthesis record](../../../../benchmarks/search/SYNTHESIS.md).
