<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Donkey Kong workload adapter

This module adapts Donkey Kong (NES, World Rev 1) to Dissonance's search
interfaces. The game logic lives in
[`nes-games/src/donkey_kong`](../../../nes-games/src/donkey_kong): the RAM
decoder, power-on setup, observations, archive key, named progress, terminal
conditions and the `Workload` implementation. It follows the
[Castlevania](../castlevania/README.md) adapter for target execution and named
progress. ROM files are never committed. The request pins the ROM by SHA-256
`226c6f698f60b0a07157bd031004820789469884615fef7bdd862c3157e89d27` (NROM, 24 KiB
program, CHR ROM).

## Stages

Each round has three stages, not four. The NES version has no pie factory.
`$53` holds the stage in play: 1 is the barrels (25 m), 3 the elevators (75 m)
and 4 the rivets (100 m). Mario clears the barrels and the elevators by
standing on the top girder, `$59` equal to 7 with `$5A` set. He clears the
rivets by pulling all eight rivets, `$C1` to `$C8`. After the rivets the round
counter `$54` goes up and the stage returns to 1.

Victory is `$54` at 1 or more: round 1 cleared. Each further stage clear is
progress, and the observation's cleared count is the round times three plus the
index of the stage.

Writing `$53` about 30 frames after Start boots that stage directly. The browser
uses this to start a search on the elevators or the rivets.

## Memory map

Addresses were read from the program's code with a 6502 disassembler and
checked by probing QuickNES from power-on and by replaying a winning tape.

| Address | Meaning |
| --- | --- |
| `$20` to `$22` | Score as packed decimal bytes: `$20` the tens and ones, `$22` the thousands and hundreds, `$21` the hundred-thousands and ten-thousands |
| `$2E` | Bonus timer in hundreds as packed decimal; it starts at `$49` and counts down |
| `$46`, `$47` | Mario's x and y; the top-left of his sprite is 5 less in each |
| `$53` | Stage: 1 barrels, 3 elevators, 4 rivets |
| `$54` | Round, counted from 0 |
| `$55` | Lives left after the one in play; 2 on a new game |
| `$58` | Attract-mode demo flag |
| `$59` | Girder floor Mario stands on, 1 at the bottom and 7 at the top |
| `$5A` | Mario is on a girder |
| `$96` | Mario's posture: 1 standing or walking, 2 on a ladder, 4 in the air, `$FF` dead |
| `$C1` to `$C8` | Rivet flags on the rivet stage |
| `$200` to `$203` | First sprite entry, Mario's: y, tile, attribute, x. Attribute bit 6 means facing left |

## Declared workload

Setup waits 100 frames, presses Start for 3 frames and waits 540 frames in
holds of at most 120. The genesis check requires the barrels, round 0, Mario at
his start position, standing, with 2 lives. Gameplay is then searched from the
bottom girder with no route injected.

The controller vocabulary is the shared set of pressable chords, without Start
and Select. The archive key orders states by the number of stages cleared, then
rivets pulled, then height above the start. The place is the stage, the stages
cleared, Mario's 16-pixel cell and his posture. A cell keeps one state, and
more bonus time, then more lives, then a higher score is preferred.

A terminal state is a dead Mario, the attract demo, or round 1.

The named progress diagnostics record the first execution that cleared each
stage and the first that finished round 1.

## Results

Seed 1 with eight workers cleared the barrels, the elevators and the rivets
from power-on in 357,933 executions and 47 seconds, and the winning tape ends
in round 1. The tape replays to the same endpoint in a second process.
