<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Mega Man 2 workload

This module owns every Mega Man 2 RAM address and game interpretation. The
generic engine receives opaque keys, typed actions, observations and
snapshots. The decoder documents RAM addresses beside their definitions in
`target.rs`.

## Modes

A whole-game search starts at Crash Man's first playable frame after the
power-on menus and ends at the ending scene or a lost life. The Robot Master
order is fixed: Crash, Flash, Metal, Air, Bubble, Heat, Wood, Quick, then the
six Wily castles. When a boss falls, the target drives the menus to the next
stage inside the same action: it waits for the weapon award, presses through
the password screens to stage select, moves the cursor to the next stage in
the order and presses Start. A castle clear waits for the next castle to load.
The action's observations end with an arrival at the new stage, so a search
action spans the whole transition. A rooted run applies its root input after
that genesis and starts from where the root ends.

A stage search starts from the power-on menus selecting one Robot Master stage
and ends when that boss falls or Mega Man dies. Its result is an independent
stage clear.

Gameplay inside a stage is searched. No route, weapon choice, obstacle target
or boss weakness is injected.

## Reading the game

The decoder reads stage, screen, room, position, posture, health, the equipped
weapon and its energy, boss and enemy damage, active platforms and the weapon
menu. Some bytes need context:

- The bank byte at `$29` says whether a menu or gameplay is running.
- The Wily 5 teleporter rooms borrow the stage byte for a Robot Master number
  for a few frames. The decoder keeps the previous stage through that borrow.
- Wily 5 tracks its refights in a bitmask at `$bc`. The Wily Machine refills
  its meter between its two forms, so boss damage reads zero during the refill.
- Boobeam Trap's cannons and barriers count while that fight is underway, as
  one bit per standing object's 32-pixel cell on the screen. The game assigns
  these objects to different slots from one attempt to the next, and the
  objects never move, so the cell names an object where the slot does not.
- A boss intro runs from the boss byte at `$b1` turning on until the boss
  meter starts to fill. Its length in frames counts as the intro's progress,
  because Mecha Dragon flies in for about 450 frames while the player has to
  stay above the pit, and nothing else in the key changes during that wait.
- Enemy damage counts only on an active object that the hit flag at `$110`
  confirms, because enemy slots are reused.

## Archive key

The progress tier is the Robot Master clears, the castle clears, the Wily 5
refights, whether the Wily Machine's first form is broken and whether the
boss that clears the current castle is defeated. A castle clear waits for the
stage-clear sequence after the kill, and a kill that left the tier unchanged
would share its draws with every fight state in the room. The place is the
stage, screen, room, boss damage, enemy damage, the 32-pixel position bucket,
posture, platforms and whether the menu is open, plus the Wily 5 refights,
the refight boss in play, the Boobeam targets left and the boss intro's
64-frame step. The holder identity is
the 16-pixel position bucket, the weapon and the menu state. Health and summed
weapon energy choose which arrival holds a slot and add no slots.

## Milestones and evidence

A whole-game run reports `named_progress` with the first try that saw each
route milestone: every stage entry, every boss fight start, every boss clear,
each Wily 5 refight, both Wily Machine forms and the ending. Rooms report as
`<stage>_room_<n>` so a stall has a named place; they do not trigger
checkpoints. Each route milestone writes its first arrival to
`campaign/milestone-inputs/<name>.json`, and the best health and best weapon
energy at that milestone to `<name>-health.json` and `<name>-energy.json`.
Every tape replays during verification.

`retained_diagnostics` carries the end-of-run census of the live archive.
`live_entries_by_screen` maps `stage:screen` to
`[entries, max health, max summed energy, selections]`, and
`live_entries_by_screen_row` maps `stage:screen:row` to
`[entries, selections]`, where a row is 16 pixels from the top. Both read
only cached active endpoints, so they are lower bounds where snapshots are
missing.

## Tools

`mm2-film` replays an input to video. `mm2-energy-probe` prints the decoded
state and the twelve weapon-energy bytes at each action endpoint; the last
byte is the energy-tank count. Both take the target first, as
`<stage> <chain-prefix.json>` or `whole-game [--root ROOT.json | --tape TAPE.json]`.
`--root` applies a root input after genesis like a rooted run. `--tape`
replays a raw input from power-on and starts wherever it ends in play. Both
tools stop once the target is terminal.

Use the common [local evaluation runner](../../../../benchmarks/search/README.md).
