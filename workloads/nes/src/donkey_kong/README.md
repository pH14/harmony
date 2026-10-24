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

The stage order is the same in every round: the program steps `$53` through 1, 3
and 4, and each wrap increments `$54`. The observation's cleared count is the
round times three plus the index of the stage, so it keeps rising through later
rounds. The goal is `$54` at 10, ten rounds cleared. The browser page also
treats round 1 cleared as a win in its view.

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
rivets pulled, then height above the start; height does not count on the rivet
stage, where the last rivet can be below the highest girder. The place is the stage, the stages
cleared, Mario's 16-pixel cell and his posture. A cell keeps one state, and
more bonus time, then more lives, then a higher score is preferred.

A terminal state is a dead Mario, the attract demo, or round 10 reached.

The named progress diagnostics record the first execution that cleared each
stage and the first that finished each round: `barrels_cleared`,
`elevators_cleared`, `rivets_cleared` and `round_1` for the first round, then
`round_2_barrels_cleared` and so on up to `round_10`, and `goal`.

## Results

Eight workers from power-on, seeds 1 to 3, with the goal at round 8 and the
rivet stage tier ignoring height: all three seeds cleared eight rounds, 24
stages, with a replayed winning tape.

| Seed | Executions | Seconds | Round 1 at execution | Round 8 at execution |
| --- | --- | --- | --- | --- |
| 1 | 8,101,837 | 1,289 | 208,200 | 8,101,806 |
| 2 | 10,605,357 | 1,652 | 935,307 | 10,605,326 |
| 3 | 6,503,664 | 1,084 | 39,798 | 6,503,633 |

Before the rivet stage tier dropped height, seed 1 cleared 14 stages in 1,500
seconds and seed 2 cleared 13 in 1,800 seconds. Both pulled the last rivet of a
round only rarely, because states near the last rivet are lower than the girder
Mario had already reached. No model of the hammer or of the clear cutscene was
needed. The winning tapes replay in the wasm QuickNES core to round 8 at frames
62,511, 63,450 and 65,545.

With the goal raised to round 10, seed 3 cleared ten rounds, 30 stages, in
7,540,152 executions and 1,220 seconds, the same run as before up to round 8,
with a replayed winning tape. The wasm core replays it to round 10 at frame
81,718.

With the goal at round 1, seeds 1 to 3 solved in 747,442, 1,693,325 and 48,034
executions. The seed 1 winning tape replays to the same endpoint in a second
process.
