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
separate from damage; its second-form meter refill is not damage. These fields
are decoder observations; the v20 archive policy remains unchanged pending the
whole-game progress audit.

The v20 key buckets position at 16 pixels. Health and weapon energy are
same-slot preferences: they choose which endpoint holds a slot and add no
slots. It removes the prototype's
rooms-visited lineage reward: returning to the same endpoint has the same key,
regardless of the number of rooms visited. Stage, room and screen bytes identify
locations; the progress relation uses boss clears and current boss damage.
Boss damage is zero until the boss loads its health, so a Wily boss that spawns
for its approach with an empty meter reads as no damage rather than a full bar,
and it is full once the phase byte reports the boss dead. A Wily boss grants no
weapon, so a cleared boss is a granted weapon or that same defeated phase byte.
Boss clears are the progress tier. The place is the stage, screen, room, boss
damage, enemy damage, the 32-pixel position bucket, posture, platforms and
whether the menu is open. The holder identity is the 16-pixel position bucket,
the weapon and the menu state.
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
The stream and checkpoint formats advance to v2 because decoded state and
encounter evidence changed; old search checkpoints must not resume under these
readings.
