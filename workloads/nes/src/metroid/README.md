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

Use the common [local evaluation runner](../../../../benchmarks/search/README.md)
for paired search comparisons and full small-campaign replay. `metroid-campaign`
also exposes the native experiment command. The source lineage is documented in
[the synthesis record](../../../../benchmarks/search/SYNTHESIS.md).
