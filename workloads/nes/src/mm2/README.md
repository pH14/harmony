<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Mega Man 2 workload

This module owns every Mega Man 2 RAM address and game interpretation. The
generic engine receives opaque keys, typed actions, observations and
snapshots. The decoder documents RAM addresses beside their definitions in
[`nes-games/src/mm2/target.rs`](../../../nes-games/src/mm2/target.rs).

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

## Actions

An action is a button chord or a weapon switch, defined in
[`nes-games/src/mm2/action.rs`](../../../nes-games/src/mm2/action.rs). Chords
hold one of nine directions with A, B, both or neither, and never press Start
or Select. One draw in 12 is a switch, so a 16-action job holds about one
switch and most of its frames still move and fire. A switch records only a
random word. The target resolves it against the decoded state when it runs:
the choices are P plus every weapon and item held with energy left, minus the
equipped one, and the word picks one of them. The target opens the menu,
presses Down to the weapon's row, turning the page on the NEXT row when the
weapon is on the other page, presses Start and waits for the menu to close.
With nothing to switch to, or outside play, a switch waits 12 frames, the
short chord hold. Replaying the word from the same state takes the same path,
so a recording replays from its actions alone.

The menu needs care because it decodes as page × 8 + cursor row, and both
values move while it animates. Start opens the menu after about 15 frames.
While it opens, the row counts through every row of the page before it
settles, and turning a page or closing the menu reads 254 for some frames.
The target presses only once the same row has read for two frames in a row,
and releases for one frame between presses so each press registers. A switch
plans 40 frames, about the 39 a switch measured from a start holding three
weapons, and gives up after 240 frames or when the menu has not opened 30
frames after Start.

## Reading the game

The decoder reads stage, screen, room, position, posture, health, the equipped
weapon and its energy, boss and enemy damage, active platforms and the weapon
menu. Some bytes need context:

- The bank byte at `$29` says whether a menu or gameplay is running.
- The Wily 5 teleporter rooms borrow the stage byte for a Robot Master number
  for a few frames. The decoder keeps the previous stage through that borrow.
- Wily 5 tracks its refights in a bitmask at `$bc`. The Wily Machine refills
  its meter between its two forms, so boss damage reads zero during the refill.
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
the refight boss in play and the boss intro's 64-frame step. The holder identity is
the 16-pixel position bucket, the weapon and the menu state. The key declares two
preferences, and each place keeps the best arrival under each. The first ranks
health, then summed weapon energy. The second ranks the three lowest weapon
energies, lowest first, then health, so it keeps the arrival whose most-drained
weapons hold the most. Random input fires whatever weapon is equipped, and a summed
total hides one weapon spent down while the others stay full, which matters
where a boss takes damage from one weapon only.

## Strategy key

[`nes-games/src/mm2/game.rs`](../../../nes-games/src/mm2/game.rs) declares
the same key through assertions as the `MegaMan2` `Game`, with the strategy in
[`mega_man.toml`](../../../nes-games/src/mm2/mega_man.toml). Its decoder carries the
hand target's per-frame context (fall run, dying run, enemy damage, boss
intro frames, arrival) in its state. A transition is a forced `Drive` move
whose steps are the hand target's menu chords, so `NesTarget` plays the same
award wait, stage-select walk, cursor path and arrival wait. Its award, settle
and walk idles are multi-frame steps of up to 120 frames. The decoder reads no
death during a `Drive`, as the hand target's transition reads none. The tier is the
preference scope, and `min(stage, 13)` and `min(platforms, 7)` keep the place
inside 64 bits. The hand target finishes the current chord before a stage
transition, while the strategy target cuts it at the transition frame, so a
witness's frame count can differ by that remainder; no key differs. The
hand target ends an action, a weapon switch included, at the frame where the
player first reads dead, as the strategy target does outside a `Drive` step,
so a rejected path charges the same frames in each.

A weapon switch is a move at the `weapon` site, declared at 1 in 12 beside
the chord tree. Its 12 options are P, the eight Robot Master weapons and the
three items, and each plays the hand target's `WeaponSwitch` menu path one
frame per step. A state offers exactly the hand's switch choices, and only
where the menu can open; with no choice the state offers no move, where the
hand target idles 12 frames. A switch answer costs the hand switch's 40
planned frames. nes-eval runs this workload as `mm2-strategy`, whole game from
power-on, and the hand workload stays `mm2`. A strategy run writes the first
input that satisfied each `sometimes` declaration to
`campaign/milestone-inputs/<name>.json`, with no stocked tapes, and
verification replays each one.

[`draw_check.rs`](../../../nes-games/src/mm2/draw_check.rs) checks that the
declared moves draw what the hand draws: through the chooser at the root and
at two prefix states, for a job's first ask under the shipped policy, and
through the harness the hand search and target share. A whole-game run with
`"shadow": true` gives each hand target a `ShadowHook`. While the shadow is
on, the hand target drives a transition with `MegaMan2::play`'s chords and
passes the step states to `forced`.

## Archive memory

An archive entry of the hand workload is 112 bytes, with a 26-byte
`Mm2ArchiveKey`, and an entry of the strategy workload is 128 bytes, with a
28-byte `AssertionKey` (`size_of` on aarch64 macOS, release build). The
strategy entry is larger because its key and its 16-byte milestones take 44
bytes, against 29 for the hand key and its 3-byte milestones. Archive history compacts once it holds more than a quarter of
`memory_mib`, so the strategy arm compacts after fewer entries than the hand
arm, and the two arms' search decisions differ from the first compaction on.
An outcome that decides a choice for this workload is therefore scored on
`mm2-strategy`. The archive charges each resident snapshot its struct size, its
compressed QuickNES state and a 32-byte allocation header, and the strategy
adds its accumulator's heap, which is empty for this strategy. The struct is 240
bytes for the strategy and 96 for the hand. Along the Wily 5 root tape the
compressed state averages 8,126 bytes, so a strategy snapshot is charged 8,398
bytes against 8,254, and the same 1.5 GiB budget holds about 1.7% fewer
resident snapshots: 191,785 against 195,131 if snapshots held the whole budget.
A preference packs into 32 bits, which holds the three lowest
weapon energies and health at a byte each. The full sorted energies would need
12 lanes and do not fit.

## Milestones and evidence

A whole-game run reports `named_progress` with the first try that saw each
route milestone: every stage entry, every boss fight start, every boss clear,
each Wily 5 refight, both Wily Machine forms and the ending. Rooms report as
`<stage>_room_<n>` so a stall has a named place; they do not trigger
checkpoints. Each route milestone writes its first arrival to
`campaign/milestone-inputs/<name>.json`. Stage entries, boss fight starts
and Wily Machine forms also write the best health and best weapon energy seen
there to `<name>-health.json` and `<name>-energy.json`, since those are the
places a later segment starts from. Clears, refights and the ending have no
stocked tapes. Every tape replays during verification.

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
