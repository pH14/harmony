<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Punch-Out!! workload adapter

This module adapts Punch-Out!! (USA, 1990), the MMC2 (mapper 9) release with
Mr. Dream as the final opponent, to Dissonance's search interfaces. The game
logic lives in [`nes-games/src/punchout`](../../../nes-games/src/punchout); this
module re-exports it and adds the native film and endpoint replay. The
adapter follows the [Twin Dragons](../twin_dragons/README.md) layout: one
`target` with the RAM decoder, setup tape and `Target` implementation, one
`archive` with the key, milestones and controller vocabulary, and one
`campaign` with the `Workload` implementation.

## ROM

The ROM is not redistributed. `nes-eval` checks the caller's file against
SHA-256 `2f06867500d5bc105cfa30f041fcb6bf5934d6b464779fbc4b041203af72b052`
(Punch-Out!! (USA), NES 2.0 header, 128 KiB PRG, 128 KiB CHR). The pinned
QuickNES core runs it.

## Memory map

The addresses were found by RAM diffs with `mm2-probe` (it reads any ROM named
by `HARMONY_MM2_ROM`) over scripted fights against Glass Joe and Von Kaiser,
checked against rendered frames.

| Address | Meaning |
| --- | --- |
| `$0000` | Mode: `$01` fight and round intro, `$FF` between rounds, `$FE` won fight by knockout and the following screens, `$FC` won fight by technical knockout (three knockdowns in a round), `$FD` lost fight |
| `$0001` | Opponent index, 0 for Glass Joe; it already names the next opponent on the won-fight screen |
| `$0002` | Screen step between fights: 0 on the result screen, 2 on the next opponent's profile |
| `$0005` | Why the fight stopped: 0 running, 1 opponent down, 2 Mac down, 3 round over |
| `$0006` | Round, from 1 |
| `$000D` | Scene between fights: `$0C` result, champion, cutscene and profile screens, `$0A` the password box |
| `$0012` | 1 while a screen between fights waits for Start |
| `$0013` | Cutscene timer; it counts down by one every 8 frames through the bicycle scene after a title bout |
| `$0090` | Opponent action; bit 7 is set on the frame an action starts |
| `$0302`-`$0305` | Clock: minutes, tens of seconds, seconds |
| `$0323`, `$0324` | Hearts as tens and ones digits; 20 at the start of the first fight |
| `$0342` | Stars, 0 to 3 |
| `$0391` | Mac's health, `$60` full |
| `$0398` | Opponent's health, `$60` full |
| `$03CA` | Opponent knockdowns in this round |

The between-fight screens and the screen between rounds wait for Start. Start
during a fight throws the star uppercut when Mac holds a star.

## Declared workload

Setup waits 300 frames for the title, presses Start on NEW GAME, waits 400
frames for Glass Joe's profile, presses Start, and waits 1,100 frames for the
round 1 clock to start. The genesis check requires mode `$01`, opponent 0,
round 1, both health bytes at `$60`, 20 hearts and no stars.

The controller vocabulary holds idle, A and B (body punches), Up+A and Up+B
(face punches), Left and Right (dodges) and Down (block, and duck on a double
tap), with short holds of 2 to 8 frames and long holds of 9 to 32. One draw in
12 is a Start tap of 2 to 4 frames, which throws a star uppercut in a fight and
advances the screens between rounds and fights. Select is excluded. A punch
lands on its press, so timing matters more than hold length.

Mac knocked down (`$0005` = 2 during a fight) and a lost fight (`$FD`) are
terminal deaths, as a Mega Man 2 death is. The search ends the rollout there
because getting up and continuing loses the round's progress. The objective is
defeating the fourteenth opponent, read as the opponent index reaching 14; no
run has reached Mr. Dream yet, so that reading is not checked against the
ROM.

The progress tier is the opponents defeated. The place is the
opponent, the phase (fight, between rounds, won, other), the screen step
between fights, the round, the opponent's knockdowns this round, the
opponent's health in 8-point buckets and the opponent's action. While the
action is the spin punch series (26) the place also holds the opponent's step
counter (`$0096`) modulo 4, so progress through the series' 265 frames is kept
as separate places. Outside a fight the place drops the fight fields and adds the scene, the Start prompt
and the cutscene timer in 16-step buckets. After a title bout the screens run
for about 3,000 frames, including a 1,530-frame cutscene, and without the
timer they form one place that the search does not leave. Higher Mac health, then hearts, then
stars is preferred within a place, and the identity adds the exact health
bytes and hearts.

Milestones are the opponents defeated, the most knockdowns of the current
opponent in one round, victory and death. The first execution that defeats each
opponent is reported as `named_progress.first_seen` (`glass_joe_defeated` to
`mr_dream_defeated`), which `benchmarks/search/milestones.py` reads. The
progress watermark is the triple of opponents defeated, knockdowns and damage
dealt to the current opponent.

## Running

`nes-eval` takes `"game": "punchout"` with no stage, ai or whole-game option,
and `nes-film` renders its witness or champion inputs.
