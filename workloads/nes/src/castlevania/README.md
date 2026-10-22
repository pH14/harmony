<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Castlevania workload adapter

This module adapts Castlevania (USA) (Rev 1) to Dissonance's search
interfaces. The game logic lives in
[`nes-games/src/castlevania`](../../../nes-games/src/castlevania): the RAM
decoder, power-on setup, observations, archive key, named progress, terminal
conditions and the `Workload` implementation. It follows the
[Twin Dragons](../twin_dragons/README.md) adapter for target execution and the
[Mega Man 2](../mm2/README.md) adapter for named progress, milestone tapes and
checkpoint evidence. Rom files are never committed. The request pins the ROM
by SHA-256 `5e859d77bc0e3a9f30a7c14d2637b9c2aae6ce19f6520a7c9b252a86974f7800`
(mapper 2, CHR RAM, NES 2.0 header).

## Memory map

Addresses were checked with `castlevania-probe` on QuickNES from power-on, by
walking right through the courtyard and the first hall until Simon died, and by
reading the HUD in screenshots. The health, boss meter, whip, hearts and
sub-weapon addresses match the TASVideos Castlevania resource page.

| Address | Meaning |
| --- | --- |
| `$18` | Game mode: 1 title, 3 after Start, 4 intro walk, 5 play, 6 dying, 7 game-over fade, `$0A` walking through a door between stages, `$0D` game over |
| `$19` | Sub-mode; in mode 5 it is 6 once Simon is in control and 0 to 3 while a stage loads |
| `$28` | Stage. 0 is the courtyard before the castle, and 1 to 18 are the numbered stages. The HUD shows stage 1 for both 0 and 1 |
| `$46` | Section inside a stage, which changes on stairs between floors |
| `$2A` | Lives; the HUD shows one less |
| `$40`, `$41` | Simon's x inside the section, low and high byte |
| `$3F` | Simon's y on screen, larger is lower |
| `$45` | Health, `$40` when full |
| `$5B` | Invincibility after a hit, counting down from `$30` |
| `$70` | Whip level, 0 to 2 |
| `$71` | Hearts |
| `$15B` | Sub-weapon, 0 when none |
| `$64` | Double or triple shot multiplier |
| `$1A9` | Enemy meter, `$40` when full. It is the boss's health in boss stages |

Simon walks one pixel a frame, and the camera keeps him at screen x `$80`
while it scrolls, so `$40` and `$41` count world pixels.

## Declared workload

Setup waits 120 frames, presses Start for 2 frames and waits 600 frames. The
genesis check requires mode 5 sub-mode 6, stage 0, section 0, full health and
lives byte 4. Gameplay is then searched from the courtyard; no route, item or
boss pattern is injected.

The controller vocabulary is 20 held chords: no input, the eight directions,
jump alone and with left or right, whip alone, with left or right, crouching
and with up (the sub-weapon), up-right and up-left with the whip for stairs,
and right with jump and whip. Start and Select are excluded. The shared
change-one-control draw moves between them, with short holds of 2 to 12 frames
and long holds of 16 to 60. Jumps are committed and the whip winds up for
about 22 frames, so a long hold is a whole jump or a whole whip.

## Terminal policy

A lost life is terminal, as in the Mega Man 2 and Metroid adapters: mode 6,
7 or `$0D`, or zero health in mode 5. The game would restart the stage, and a
search continues only from states where Simon is alive. Victory is stage 18
with the game in any mode other than play, dying, door or game over, which
happens only after Dracula's second form falls. This victory rule and the
boss-defeat reading below are untested against the game, since no probe has
reached a boss.

## Archive key

The tier is the stage, whether the stage's boss meter reads zero, the whip
level, whether a sub-weapon is held and the hearts bucket (0, 1 to 4, 5 to 9,
10 or more). The place is the section, a play-or-transition mark, the boss
damage in steps of 8, and the 32-pixel cell of x and y. Door transitions and
stage loads are places keyed by mode and sub-mode, so the walk between stages
is not a dead end. The holder identity uses 16-pixel cells. One preference
ranks health, then hearts, then lives.

## Milestones and evidence

A run reports `named_progress` with the first try that saw each route
milestone: `stage_NN_entered` for stages 1 to 18, `<boss>_engaged` on the
boss meter's first drop and `<boss>_defeated` when it reads zero or the next
stage is entered, for `phantom_bat` (stage 3), `medusa` (6), `mummies` (9),
`frankenstein` (12), `death` (15) and `dracula` (18), and `ending`.
`stage_NN_section_S` reports each section reached and triggers no checkpoint.
Each route milestone writes its first arrival to
`campaign/milestone-inputs/<name>.json`. `retained_diagnostics` counts live
entries by `stage:section` as `[entries, max health, selections]`.

## Tools

`castlevania-probe <buttons:hold|pADDR=VALUE,...> <every> <addr,...>` runs a
schedule from power-on and prints the listed bytes every `every` frames, or
whenever one of them changes when `every` is 0. A `pADDR=VALUE` step writes one
RAM byte. `PROBE_DIFF=1` prints every zero-page change, and
`PROBE_SHOTS=<dir>` with `PROBE_SHOT_EVERY=<n>` writes raw RGB frames. It reads
the ROM from `HARMONY_CASTLEVANIA_ROM` and the core from
`HARMONY_QUICKNES_CORE`. `nes-film --game castlevania` renders an input to
video.

Use the common [local evaluation runner](../../../../benchmarks/search/README.md)
with `"game": "castlevania"`.
