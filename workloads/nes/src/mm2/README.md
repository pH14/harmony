<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Mega Man 2 workload

Mega Man 2 is a game declared through assertions: the `MegaMan2` `Game` in
[`nes-games/src/mm2/game.rs`](../../../nes-games/src/mm2/game.rs) decodes the
RAM, declares the assertions and plays the scripted transitions, and the
strategy in [`mega_man.toml`](../../../nes-games/src/mm2/mega_man.toml) keys
the archive. `NesWorkload<MegaMan2, M>` is its `Workload`, and this package
names the QuickNES one `Mm2Workload`. The decoder documents RAM addresses
beside their definitions. nes-eval runs it as `mm2-strategy`, either the whole
game from power-on (`whole_game`) or one Robot Master stage (`stage`).

## Modes

A whole-game search, `MegaMan2::WHOLE_GAME`, starts at Crash Man's first
playable frame after the power-on menus and ends at the ending scene or a lost
life. The Robot Master order is fixed: Crash, Flash, Metal, Air, Bubble, Heat,
Wood, Quick, then the six Wily castles. When a boss falls, the state declares
a forced `Drive` move that runs inside the same action: it waits for the weapon
award, presses through the password screens to stage select, moves the cursor
to the next stage in the order, presses Start and waits for the arrival. A
castle clear waits for the next castle to load. The award, settle and walk
idles are multi-frame steps of up to 120 frames, and the decoder reads no
death during a `Drive`. The action's last observation is the arrival at the
new stage, so a search action spans the whole transition. A rooted run applies
its root input after that genesis, and `rooted_genesis` requires the root to
end in play after a stage arrival and outside a drive.

A stage search, `MegaMan2::at_stage`, starts from the power-on menus selecting
one Robot Master stage and plays that stage's cursor path and arrival wait,
under the identity `mm2-stage-N-v1`. Its strategy replaces the `ending` goal
with that Robot Master's defeat, which holds once the weapon award sets its
bit, so the win ends there instead of after the walk to the next stage. Its
result is an independent stage clear. The Crash stage start plays the
whole-game setup under its own identity.

Gameplay inside a stage is searched. No route, weapon choice, obstacle target
or boss weakness is injected.

## Actions

An action is a button chord or a weapon switch. `MegaMan2::CHORDS` holds one of
nine directions with A, B, both or neither, and never presses Start or Select.
A weapon switch is a move at the `weapon` site, declared at 1 in 12 beside the
chord tree. Its 12 options are P, the eight Robot Master weapons and the three
items. A state offers P plus every weapon and item held with energy left,
minus the equipped one, and only where the menu can open; with no choice it
offers no move. A switch answer plans 40 frames, about the 39 a switch
measured from a start holding three weapons.

[`nes-games/src/mm2/switch.rs`](../../../nes-games/src/mm2/switch.rs) plays a
switch one frame per step. It opens the menu, presses Down to the weapon's
row, turning the page on the NEXT row when the weapon is on the other page,
presses Start and waits for the menu to close. The menu needs care because it
decodes as page × 8 + cursor row, and both values move while it animates.
Start opens the menu after about 15 frames. While it opens, the row counts
through every row of the page before it settles, and turning a page or closing
the menu reads 254 for some frames. The switch presses only once the same row
has read for two frames in a row, and releases for one frame between presses
so each press registers. It gives up after 240 frames or when the menu has not
opened 30 frames after Start.

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

The state carries the context a frame alone lacks: the fall run, the dying
run, enemy damage, boss intro frames and whether the stage has been arrived
at. Mega Man is dead at zero health, after 30 frames dying, when a life is
lost, when he leaves the screen during a boss fight, or after a fall of more
than 256 pixels on a steady screen.

## Archive key

The progress tier is the Robot Master clears, the castle clears, the Wily 5
refights, whether the boss that clears the current castle is defeated, whether
the Wily Machine's first form is broken, and the screen. A castle clear waits
for the stage-clear sequence after the kill, and a kill that left the tier
unchanged would share its draws with every fight state in the room. The screen
is the last tier item, so within the same clears a higher screen of a stage is
a higher progress rank and draws favor the furthest screen reached. This relies
on the screen byte not falling along a stage's route. The place is the stage,
room, boss and enemy damage in steps of two, the 32-pixel position bucket,
posture, platforms and whether the menu is open, plus the Wily 5 refights,
the refight boss in play and the boss intro's 64-frame step. `min(stage, 13)`
and `min(platforms, 7)` keep the place inside 64 bits. The holder identity is
the 16-pixel position bucket, the weapon and the menu state. The tier without
the screen is the preference scope, and each place keeps the best arrival under each of two
preferences. The first ranks health, then summed weapon energy. The second
ranks the three lowest weapon energies, lowest first, then health, so it keeps
the arrival whose most-drained weapons hold the most. Random input fires
whatever weapon is equipped, and a summed total hides one weapon spent down
while the others stay full, which matters where a boss takes damage from one
weapon only. A preference packs into 32 bits, which holds the three lowest
weapon energies and health at a byte each. The full sorted energies would need
12 lanes and do not fit.

## Archive memory

An archive entry is 128 bytes, with a 28-byte `AssertionKey` and 16-byte
milestones (`size_of` on aarch64 macOS, release build). Archive history
compacts once it holds more than a quarter of `memory_mib`. The archive
charges each resident snapshot its struct size, its compressed QuickNES state
and a 32-byte allocation header, plus its accumulator's heap, which is empty
for this strategy. The struct is 240 bytes. Along the Wily 5 root tape the
compressed state averages 8,126 bytes, so a snapshot is charged 8,398 bytes.

## Milestones

A run reports `named_progress` with the first try that saw each `sometimes`
declaration: every stage entry, every boss fight start, every boss clear,
each Wily 5 refight, both Wily Machine forms and the ending. The first input
that satisfied each one is written to `campaign/milestone-inputs/<name>.json`,
and verification replays each one.

## Tools

`nes-film --game mm2-strategy` renders a recorded run to video, with `--stage`
for a stage run. `nes-rekey mm2-strategy RUN_DIR [STRATEGY.toml]` re-keys a
recorded whole-game run with no emulator. `mm2-probe` runs a button schedule from power-on on
any ROM named by `HARMONY_MM2_ROM` and prints the chosen RAM addresses.

Use the common [local evaluation runner](../../../../benchmarks/search/README.md).
