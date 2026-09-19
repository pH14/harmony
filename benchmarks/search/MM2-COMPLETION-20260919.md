# Mega Man 2 completion investigation

Deadline: 2026-09-20 12:00 UTC (08:00 ET). Fork: `codex/mm2-completion-20260919`, starting at `5d9a3368f`.

## Current verified result

Completed: a continuous power-on controller tape clears the game through
Alien and the ending credits. The exported `completion-input.json` contains
6,800 actions. Independent macOS and Linux QuickNES replays both consume every
action, produce 254,990 frames, and match the endpoint. Independent film review
confirms the final defeat, Wily reveal, ending montage, and credits including
“THANK YOU FOR PLAYING…”. Mega Man retains HP6 and two lives after the win.

The strongest interventions so far were encounter-specific state decoding,
preserving irreversible encounter progress, and recovering the ammunition
needed for the next fight. A paired 100k probe increased verified second-form
damage from 2 to 8 after distinguishing the Machine's shell break and refill,
but could not clear from a four-health root. After ordinary Continue, another
refight clear, and a normal retry, full health and Crash ammunition enabled a
436-execution clear. That last comparison changes several conditions and is
not a causal speedup estimate. Controller banks and manual route composition
contributed to the resulting tape; it is not a fresh autonomous full-game
search result.

## Evidence and scope

The objective is a continuous, independently replayed game completion. Rooted probes diagnose a wall; their successful suffixes must compose with the actual power-on prefix. Stage clears alone are not completion. Review film before interpreting archive counters. Preserve both discovery witnesses and surviving continuations.

The historical input `/root/mm2/game/10-wily3/next-prefix.json` on msr1 has SHA-256 `341537467113fa3eea71da63b831d567fb565cc3c2b7163f6fc782d7b44f6192`. The current `nes-progress` independently replayed it twice to Wily 4, with ordinary setup adding 1616 frames. Both endpoints agreed: stage 11, screen/room 22, health 28, eight weapons, four lives. This is retained search evidence, not a new full search result.

Lab source and runs: `/root/mm2-completion-20260919/`. ROM SHA-256 `49136b412ff61beac6e40d0bbcd8691a39a50cd2744fdcdde3401eed53d71edf`; lab core SHA-256 `5a65587bf6faa5bc86ea05648b81b0e01e5f639ea5020166a14b5d96a92a3db0`. The core binary differs from the old Mac audit; ROM, input, and decoded endpoint agree. Snapshot hashes are not assumed portable across different core binaries.

## Initial experiments

1. Render the old Wily 4 seed-1 6M champion from the verified prefix, inspect the actual blocked transition, and check applied versus recorded actions.
2. Current-code Wily 4 baseline: seed 1, 100,000 executions, eight workers, 4096 MiB archive budget, action limit 8192, alphabet-only mixture. Capture film and frontier evidence at this short budget before extending.
3. Audit late-game adapter correctness, especially multiple encounters in Wily 5 and actual final completion detection, before treating a boss signal as victory.

The historical 6M suffix rendered all 583 recorded actions (23,078 frames),
ending alive. Its stage-relative SHA-256 is
`502774f4a7d83814fdf4a08317e166939e095dc4b5c5eac709b285a5b6be68fe`.
Resource diagnostics are retained separately from visual interpretation in
`runs/historical-wily4-energy.txt` on the lab.

Rooted diagnostic support was added as `--root-input`. A 32-execution smoke
run from the historical suffix passed headless/video endpoint agreement;
all 17 existing MM2 tests and `cargo clippy --bin mm2-campaign -- -D warnings`
passed on the lab. An independent raw-input composition check is still
required before treating a rooted extension as new continuous progress.

Follow-up short probes: 100k executions at the old suffix endpoint (six
workers, alphabet-only), and a 100k stage-start comparison with
`energy_splice:6` (four workers). Worker counts differ, so this is an
initial screen, not a controlled causal comparison of input policies.

## First verified obstacle

Film identifies a normal Sniper Armor encounter at screen 49 / room 34,
not a boss. The historical tape ends with the player to its left. The rooted
100k archive retains 4,346 states in this room, including all twelve weapon
selections, but every retained key has zero enemy damage and none has an x
bucket above the starting bucket 6. Controlled firing probes use actual
searched weapon-switch states and raw power-on replay. Their starting
positions differ, so they diagnose observability rather than rank weapons.

The independent Mac `mm2-replay` utility consumed all 4,768 composed actions
and reached the lab's historical endpoint in 173,543 frames without adapter
setup or terminal checks. Its local evidence is under
`/private/tmp/mm2-completion-20260919/` (`raw-tail*` and
`historical-wily4-full.json`).

The adapter audit found that the death-animation counter reset on every
action. It is now serialized with observations and snapshots. Existing
library/all-target tests and lint checks passed in the isolated review
clone; the integrated build also accepts the known-good historical root
and passes the 32-execution replay smoke check. Actual action-segmentation
and snapshot-restore integration checks remain in progress. The corrected
adapter is repeating both 100k probes at their previous worker counts.

The controlled w0 firing tape starts in player state 0 with health 8, then
loses a life and respawns; film confirms death, not a successful attack.
The w1 tape starts airborne, lands, loses all health, and respawns without
a visible hit on the Armor. These are not evidence of an enemy decoder bug.
They show that legacy archive keys alone do not establish viable starts.

Next short arms: corrected historical root with entry threshold 64 (same
seed, six workers, 100k budget as corrected threshold-3 control); and an
earlier root truncated after action 582, before the final landing, with
threshold 64 and four workers. Both roots use actual recorded inputs.
The old Wood-stage notes specifically credit a change from three entry
retries to 64 for learning a precise jump-and-shoot event; that is prior
evidence for this ablation, not a claim it solves Wily 4.

The ignored real-ROM integration test now passes on the Mac: original
versus one-frame segmentation has identical first-death timing, restoring
a snapshot during the death timer preserves that timing, and the separate
known-surviving flash fixture remains alive. Fixtures are `test-root450`,
`test-death`, and `test-flash` JSON files in the local evidence directory;
the test log is `dying-integration.log`.

The w7 firing probe survives on a ledge (x=28, y=52, health=8, lives=4),
with the Armor still active. An apparent health-byte decrease in inactive
slot 4 coincides with an object-ID change and is not evidence of enemy
damage. The next root uses the actual searched weapon-switch prefix plus
60  neutral frames to settle, preserving ammunition instead of including
the diagnostic firing sequence.

## Resource plan

msr1 has twelve CPUs and about 40 GiB available memory at start. Use bounded runs and retain headroom for replay and builds. The Mac has 64 GiB physical memory but substantial compression; keep local work modest and never exceed the authorized eight cores. Avoid copying multi-gigabyte historical archives when tapes suffice.

## Interpretation constraints

Historical preference-first, dominance-only, and long continuation pilots lost depth. Do not repeat them as untested ideas. Summed weapon energy is insufficient to establish availability of a particular required weapon; inspect actual energy vectors when film identifies the relevant action. A progress watermark ordered by screen/room can hide meaningful alternate routes.

## Confirmed combat-observation defects

A raw one-frame replay of the recorded Leaf Shield probe establishes a first
hit at power-on frame 173684: Sniper Armor slot 31 remains object 0x4e,
spawn index 44, and its HP changes from 20 to 6 with hit flag 1. Mega Man
still has eight HP. He loses all HP five frames later; the later enemy kill
is not a surviving victory. The old detector incorrectly treated initial
20 HP as an idle sentinel. The corrected v20 detector requires a prior active
enemy, a confirmed hit, and preserved identity or the recorded killed-object
transition. Inactive slots and reused projectile slots do not count.

The exact-hit smoke exposed a second defect: the ordinary endpoint fallback
redecoded RAM and discarded accumulated enemy damage. The counter now survives
that path. The first-hit root (`armor-first-hit-root.json`) has 624 real
stage-relative actions; its initial damage is four, represented by bucket two.
The corrected 32-execution smoke retains that bucket in its first children.
All 133 library tests passed. A real-ROM boundary/reset/snapshot regression
and independent video endpoint tracking are being validated separately.

The first corrected historical-root run used the same seed 1, six workers,
100k executions, 4096 MiB budget and entry threshold 64 as its v19 comparison.
It finished at 11,790,048 emulated frames. Its champion film is not its furthest
physical witness: archive entry 48129 reaches x bucket 12 / y bucket 7 on
screen 49 with eight HP and a closed menu. This requires film and landing
verification before it is a viable continuation. A separate exact-hit-root
run tests local feasibility; it does not test discovering the hit.

`armor-heatmaps.png` and `armor-spatial-summary.json` in the local evidence
directory show retained screen-49 position counts, maximum health, and
cumulative selections to surviving entries. These are not total or recent
selection shares. The prior e64 historical-root run covers the right half of
the room, whereas all three armed-root admission/retirement arms mostly cover
the left third. Reported excursions to earlier rooms are backtracking, not
forward passage. The admission-probed champion's screen-50 endpoint has
`dying_run=26`; its viability remains unverified.

The combat correction is committed as `1733883c`. The real-ROM regression
passes held versus segmented inputs, snapshot restore, reset, and rendered
endpoint equality. A qualified 32-execution warm start imported ten checkpointed
entries with no rejection and passed report/checkpoint replay equality; both
video and headless endpoints preserve damage four. Successful lab evidence is
`first-hit-resume-persist-smoke`.

The reconstructed entry-48129 tape has 49 suffix actions, not merely its last
`input_suffix`: its parent stores a compacted full `input.actions` path.
It exactly reproduces x=192, y=126, health=8, weapon=2. All 120 fixed motion
probes then die, usually by contact within two frames. It is not a usable
waypoint. The bank tests twelve basic button combinations at ten durations,
then 60  neutral frames; this is bounded negative evidence, not a proof that
no continuation exists. Film and earlier takeoff-state probes remain useful.

## Continuous replay and usable roots

The exact-hit feasibility champion visibly destroys the Armor and spawns its
Joe pilot while Mega Man remains alive at eight HP. The resulting stage-relative
`armor-killed-root.json` has 636 actions. It admits 80/120 simple motion probes
with a 60-frame holding tail, although all survivors remain at or left of x104.
Button-edge control matters: the root ends with A held, so the subsequent bank
also tests an explicit four-frame release before jumping. A visually empty
right-side region still blocks motion; interpreting that geometry is unresolved.

A separate transfer replays the known climb from seed 3 entry 37233, reaching
the encounter with 14 HP and no lost life. The full raw tape is
`candidate-tail573-582.json`; the stage-relative root is `armor-health14-root.json`
(717 actions). Transferring the old high-ledge approach from there reaches
x27/y52 with 14 HP (`health14-ledge-root.json`). These are actual controller
trajectories, not edited RAM. Their short searches have not yet cleared Wily4.

The original raw utility still restored an emulator snapshot between actions.
Version two instead stages the entire tape at power-on and runs to successive
action deadlines without intermediate restores. The historical 4,768-action
173,543-frame tape agrees at its endpoint and all 13 previously traced final
actions. The 4,821-action Armor-kill tape independently reaches the same eight-HP,
four-life endpoint after 174,105 frames. Local reports are
`historical-continuous-report.json` and `killed-continuous-report.json`.
This rules out intermediate restores as the source of these particular results.

A reported summed energy of 307 is not a normal refill. Continuous replay of
health14 seed 1 entry 47074 yields Atomic Fire energy 251, with the remaining
vector `[20,1,0,0,0,28,6,0,1,0,0]`. It is an observed underflow-like game state,
not evidence that all weapon meters were replenished. Do not interpret summed
energy alone as the resources needed to finish the stage.

## Coordinate-domain diagnostic

The historical late-stage states have a growing discrepancy between player
world screen (`$440`) and camera screen (`$20`). At action 166 these are both
31, with player x 152/y 17. Action 167 uses down+A+B for 12 frames and ends at
player screen 32/x 88/y 23 while the camera stays 31/x 0: a 192-pixel world
displacement. By action 198 the player is 35 and camera 33; at the historical
endpoint they are 49 and 34. Raw `$37` is zero at those endpoints. `$1b`, named
`camera_state` in the adapter, is actually a screen-update flag in the
disassembly; it must not be treated as the complete scrolling state.

An initial sparse film review interpreted these as ordinary edge and ladder
transitions. That is insufficient: the sprite routine computes the high-byte
screen difference but does not use it in its subsequent low-byte drawing
coordinates. A wrapped visible sprite can therefore conceal world/camera
divergence. Frame-by-frame inspection, background comparison, and collision
source are being checked before treating any coordinate bound as an invariant.

Splitting action 167 into one-frame actions preserves the continuous endpoint
and confirms exactly 16 pixels of rightward movement on every frame, despite
no horizontal button. Evidence: `first-divergence-frames-trace.jsonl`.

The default-policy control rooted at historical action 148 completed 100k
executions on the lab, seed 1, six workers, threshold 64. It retained 46,685
entries; 10,087 have a closed menu and equal player/camera screens. The deepest
such entries are room 33, whereas the reported maximum player screen is 42 and
maximum camera room 35. These are archive counts, not verified destinations.
Candidate 30142 has equal screen/room 33, 12 HP, and a 265-action composed stage
root, but continuous replay shows that this is an upward scrolling transition
back toward camera 32, not a stable aligned arrival at 33. Its 100k continuation
retains 54,060 entries, only 116 with equal coordinates, and no stable forward
arrival has been established. This invalidates selecting a waypoint from equal
coordinate fields alone. A default-off, identity-tagged
constraint on idle-scroll coordinate gaps is a diagnostic ablation,
not a proven rule of the game.

The paired 100k diagnostic runs reduce serialized entries with gaps above one
from 25.7% to 0.6% on lab seed 1 and from 20.8% to 0.6% on Mac seed 2. The
remaining exceptions include active scrolls; these counts include ancestry,
not just live representatives. `coordinate-ablation-heatmaps.png` records the
comparison. The diagnostic is committed as `b0364c84`; 135 library tests and
a qualified 32-execution checkpoint/report/video replay passed.

Lab candidate 68877 initially looked better at room 33/room 33 with 20 HP, but
its final down-right+B114 action ends at x232/y221 with `dying_run=12`.
All 90 ladder-aware suffixes die from that endpoint. Truncating the same tape
to 236  stage actions gives x125/y36, player state 5, `dying_run=0`, 20 HP and
four lives. This earlier root survives 78/90 short branches with a 60-frame
neutral tail; all eight longer checks also survive 180  neutral frames. This
is the usable room 33 foothold, not the archive's later apparent position gain.
The exact root is `candidate-68877-root-236.json`, SHA256
`b6d724a2b9fb3b44d5b029175bc21f2efdd49c89826c1fa1b665a74782154540`.

## Menu decoding correction

The disassembly names `$04` as scratch. A recorded Start/idle/Start/idle
probe at the action 148 root visually opens the menu at probe frame 4 and
closes it at111. `$29` stays `$0d` throughout frames 4–110 and `$0e` outside
that interval. The old `$04==3` decoder misses the opening animation and
falsely reports menu row 15 at frames 176,178,180,184 after closing. These
later frames include a pit-death sequence: positive HP does not imply survival.

The v21 decoder uses the menu bank byte and retains the cursor/page mapping.
The same continuous tape now recognizes exactly frames 4–110 and produces
no false menu labels after closing. A separate stage-genesis probe reproduces
the bank/menu association; Start from the dying historical endpoint does not
open a menu. This is evidence for the pinned ROM/core and frame protocol,
not a claim that bank bookkeeping is a universal game-mode interface.
All 136 library tests and the v21 qualified 32 replay pass. Evidence lives in
`menu-diagnostic-v21-trace.jsonl`, `bank-menu-qualified32`, and `menu-audit/`.

Source references are `home/nmi_wait.asm`, `home/open_menu.asm`,
`engine/menu.asm`, and `home/sprites.asm` in the lsmmega/mm2 disassembly.
The Wily4 starting energy vector is `[28,28,25,24,1,0,28,6,0,1,0,0]`.
Crash is weapon 8/vector index 7, not the Time Stopper slot. Its six units
permit one four-unit shot. Fixed large weapon capsules and enemy drops can
refill the equipped weapon; a resource route is being tested independently.


## Actual Boobeam encounter and resource recovery

A 100k continuation from the verified 236-action room 33 root reached the
actual Boobeam Trap room (38). Continuous power-on replay and film show five
green traps, gray barriers, and blue projectiles. The composed tape has 4,520
actions (`stable33-v20-boss-full-input.json`). It arrives with too little health
and only six Crash units. One shot reduces Crash to two; its delayed explosion
destroys barrier slot 27 at action 4510. Reading only the launch frame would
incorrectly classify that shot as a miss. The target's accumulated enemy damage
is four; the raw mechanical decoder intentionally does not accumulate it.
No Wily 4 victory has been established.

Real menu inputs select Crash before collecting the two fixed starting weapon
capsules, raising its stock from six to 26. The resulting continuous tape is
`crash-capsule-full-composed.json`. Normalizing the weapon and aligning timing
allows the old route to reach room 33 with 16 HP and Crash 26, then room 38
with two HP. That transfer dies after its first shot. The Armor corridor at
room 34 consumes eight HP, motivating a separate combat probe before entry.

A second verified resource option is normal Game Over followed by Continue.
It restarts Wily 4 at screen 22 with full health, three lives, and all eleven
weapon/item meters at 28. No memory edits are involved. The 4,589-action
`continue-fullstock-prefix.json` ends on the first stable standing frame.
A bank of six timing offsets transfers the old 236-action route only with
16 neutral frames first, yielding room 33, x125/y36, 18 HP and three lives.
The exact root contains 252 actions (16 separate neutral inputs plus 236).
Appending the old 99-action champion suffix loses a life: newly available
item energy changes the actions' physical effects. Fresh short searches from
this full-stock foothold are testing the downstream route instead.

Continue leaves boss health at 28 while boss phase is zero. Commit d548c951
makes encounter reporting depend on an active or defeated boss, and advances
the campaign stream format to v3. All 137 NES library tests pass. Earlier
full-stock run metadata reporting a boss at ordinary rooms is invalid; actual
room 38 entry above was independently filmed.


The stable full-stock room 33 root additionally survives 240 neutral frames
in continuous film, with HP18 and Crash28. Two 100k continuations both reach
the actual boss. The Mac's HP12 arrival has Crash0, demonstrating why the
summed-energy preference is inadequate for choosing a boss root. The lab's
entry 23833 has HP12 and an active Item2 platform. Waiting 240 frames lets
that platform expire; menu inputs then select Crash with all 28 units intact.
A further 60 neutral frames return the post-menu state to standing without
health loss. The resulting 336-action stage root is
`fullstock-boss-hp12-crash28-stable-root.json`, paired with the 4,589-action
Continue prefix. A focused pair of 100k fight probes is running from it.

`mm2-snapshot-inspect` (900f794b) exposes full retained mechanical states and
recorded death status rather than relying on the coarser archive key.
Its status filter is only a candidate-selection aid: passing it does not
establish future survival, and post-menu state-zero transients can be excluded.


The focused boss probes produce stronger partial fights, pending target-by-target
film audit. Lab seed 1 entry 29238 has boss HP10, player HP4, Crash8, and
accumulated enemy damage24; it survives 60 neutral frames. This corresponds to
three six-point trap removals in the health counter. Mac seed 2 entry 1529 has
boss HP16, player HP4, Crash12, enemy damage20, and a non-dying snapshot.
Separate 100k continuations from these two partial fights test whether their
remaining resources and geometry permit completion. A shot-by-shot film review
will distinguish which traps and barriers were removed; equal damage totals
need not mean equal remaining opportunities.


## Economical shot repair

The Mac continuation reached four removed traps but had no Crash ammunition
left. Continuous raw replay pinpoints the seven shots: barrier27; barriers25
and26 together; trap24; trap23; barrier29; trap20; trap21. Trap22 remained.
The obstacle is therefore an ammunition-feasible route, not just more shots.

Removing B at full action4961 preserves the fifth shot while keeping a route
to x100/y96. Straight jumps hit an overhang; a 14-frame left approach followed
by two frames of left+jump clears its edge. Six further jump frames and a
one-frame left+B shot remove trap21 and barrier29 together. Continuous film
`boss-left-double-success.mp4` verifies both removals and the final state at
full action5006: HP4, Crash8, boss HP10, x86/y180, lives3. Remaining targets
are trap20 at (20,96), trap22 at (172,64), and barrier28 at (120,47).
The exact stage root is `boss-left-double-success-root.json` (418 actions),
paired with the 4,589-action Continue prefix. This is a controller-only repair;
no emulator state was edited.

A Wily4-only representation ablation is being validated: preserve the active
trap/barrier identity mask and available Crash shots in retention groups.
The previous damage count can merge different remaining target arrangements,
and summed weapon energy does not imply usable Crash ammunition. This is a
hypothesis pending matched rooted runs, not yet a measured search improvement.


## Verified Wily 4 clear and Wily 5 prefix

Reusing the left climb with a 48-frame offset removes trap20 while preserving
HP4 and Crash4. The final right-side climb requires another precise ledge-edge
jump. Moving its shot earlier avoids the contact hit that the old route had
absorbed. A bounded bank finds multiple timings that kill trap22 and advance
to stage12 without losing a life.

`wily4-clear-to-wily5-full.json` is a 5,090-action continuous power-on tape.
Independent film shows the final trap explosion, castle transition, and Wily5
teleport room. Raw trace first records a stable Wily5 standing state at action
5081: stage12, room/screen24, x128/y148, HP28, lives3, weapon0. The frozen
5,082-action prefix and its hash/endpoint manifest are checked in under
`exports/mm2-completion-20260919/`. Normal deaths and Continue earlier in Wily4
are part of this same controller sequence. The game is not yet complete.

The Wily4 representation ablation is committed as a5c400be. All 140 library
tests and a qualified 32-execution replay pass. The clear above was obtained
by rooted search plus controller-sequence repairs; it does not establish that
v22 improves discovery rate over v21. Preserve that distinction when drawing
searcher conclusions. Wily5 refight-state representation is the next task.


## Wily 5 teleport correctness and refight identity

A 20k v22 diagnostic stopped every tested rematch entry. One-frame continuous
replay shows why: entering Crash Man's portal temporarily borrows raw stage7
for 48 frames while loading its tileset, then returns to stage12. HP28 and
three lives remain intact. The target's stage-regression rule misclassified
this transition as terminal. The fix interprets that byte as Wily5 only with
a trusted Wily5 genesis, raw stage below8, teleport pose11, and boss phase0.
The raw replay decoder remains literal; emulator memory is never changed.

v23 retains the exact Wily5 refight mask and the active boss identity, ranks
by completed-refight count, and does not call an intermediate refight a final
stage victory. Intermediate clears also skip automatic award-settling, since
returning to the hub requires controller input. Stream/checkpoint format is v5.
All six recorded teleport branches that v22 rejected now apply their full
240 actions, surviving at stage12/room25 with HP28/lives3. A qualified
32-execution campaign passes replay verification. All 146 NES library tests
pass, as does formatting verification.

The first refight has a separate continuous witness:
`wily5-crash-cleared-prefix.json` contains 5,400 actions, SHA256
`bf1f3e6c37877c46d09fed7027e26237276b9bcae5895e6665c99e5211da297b`.
Film verifies Crash Man's explosion and the return to the hub. The endpoint
is stage12, room24, x224/y196, HP28, lives3, Air22, refight mask128.
The controller bank's resource readout differed from the composed replay;
the continuous replay is the acceptance evidence. The remaining seven
refights, Wily Machine, and Alien are still outstanding at this checkpoint.


## All refights and Wily Machine's two forms

A bounded v23 campaign from the Crash-cleared hub reaches all eight refight
bits within 100k executions. Its continuous power-on replay and independent
film review confirm Quick, Flash, Heat, Metal, Bubble, Air, and Wood defeats
in that order, with no additional death. The route reaches Wily Machine at
HP14. The all-refights prefix contains 5,835 actions and is checked in below.
Two refight completions restore 10 health; source review could not locate
the complete reward logic in the partly opaque AI, so do not generalize that
observation to every defeat.

A normal death at Wily Machine preserves the eight refights and retries at
room40, x128/y148, HP28, lives2. The exact full-health prefix contains 5,844
actions. Life retry preserves weapon stock. Game Over Continue is a separate
path that refills all weapon meters; the skip-refill branch in the source
applies to ordinary Wily stage advancement, not Continue. This distinction
was verified against the earlier full-stock Wily4 witness.

A 100k full-health Machine search reaches 10 damage. A small controller bank
of timed Buster shots and jumps reaches 24 damage while alive at HP4. A
100k continuation from that state reaches the shell-break transition.
The first form's health becomes 1, then refills to 28 while AI state `$B1` stays 4.
AI states 5 and 6 correspond to the exposed second form in the continuous film.
Ranking the refill as 27 damage rewards a transition animation; omitting form
identity also lets the new fight collide with old first-form states.

v24 adds a narrowly gated shell-broken bit (stage12, boss12, AI states4..fd),
retains it at every depth, and ranks it before damage. State4 contributes zero
damage while the second bar loads. The raw trace now also exposes boss RAM,
object temporary bytes, and difficulty for auditing such transitions. All 148
NES library tests and a qualified 32 replay pass.

The matched v24 root/seed3/workers6/100k run retains a surviving second-form
state with eight damage. A 6,018-action continuous replay confirms HP4, boss
HP20, AI state6, x83/y132, lives2. This is a new continuation root, not a game
clear. The matched v23 snapshot audit finds only two points of actual post-refill
damage, versus eight in v24. v23 retains 15,673 refill snapshots and 2,646
post-refill snapshots; v24 retains 3,619 and 9,467 respectively. This is one
paired seed at 100k executions, not a general speedup claim. Executed frame
work is 10,885,111 versus 10,123,327. The compact comparison manifest records
the exact artifacts and filters. Two further 100k continuations from the
eight-damage root fail to improve it; sampled escape probes also fail. It is
alive at the recorded endpoint but poorly positioned for continuation. The old progress watermark/champion
ordering omits form identity, so inspect decoded archive states rather than
using its published champion to measure this comparison.


## Recovering ammunition for Wily Machine

The zero-Crash route inherited Wily4's exhausted supply. A normal Wily5
Game Over/Continue refills every meter to 28 and resets the refights. The
5,958-action fullstock prefix ends at hub24, x128/y148, HP28, lives3. Film
confirms the ordinary Continue menu and Wily5 READY transition, not a new
game. Replaying the old refight suffix with 32 timing offsets yields no
all-refights survivor; richer resources do not alone guarantee a transferable
controller route.

A fresh bounded v24 seed2 campaign does clear all eight again. Continuous
replay reaches refight mask255 with Crash28 and Bubble28 still intact. A
normal death at the machine preserves that progress and refills health.
`wily5-machine-crash28-retry-full.json` has 6,413 power-on actions, SHA256
`f8fef792c812626c71e8718ead8cde0da21e7947f2842fa4b10bb29f61cf3eb1`,
ending in room40, x128/y148, HP28, lives2, all refights cleared, Buster selected,
and total weapon energy 228. The next fight can use Crash ammunition without
sacrificing the Bubble supply needed for Alien. No memory intervention was
used in either recovery.


## Verified Wily Machine clear and Wily 6 arrival

With full health and Crash28 selected before entering the fight, the lab v24
search clears Wily Machine in 436 executions (60,675 emulated frames of search
work). An independent small controller bank also clears both forms. Its
6,457-action power-on tape ends at stage13, HP22, lives2, Crash12, Bubble28.
Film verifies the intact machine, shell break, exposed form, final explosion,
and castle transition without a death in the fight segment.

Appending neutral input reaches Wily6's first stable standing state at full
action6467: stage13, room/screen25, x128/y180, HP28, lives2. The frozen
6,468-action prefix is exported with its endpoint manifest. The game is not
yet complete: Alien and the ending remain. The resource recovery and chosen
weapon changed together; the 436-execution result is not an isolated estimate
of either one's effect.

## Alien approach and mandatory waiting

The first Wily6 100k probe reaches Alien on a 61-action suffix, but the
screen-maximizing champion follows a different cave trajectory. The actual
boss witness enters room31 at HP8. Film confirms the Alien intro and natural
retry; death returns to the cave start rather than a nearby boss checkpoint.
A menu-only repair of that witness equips Bubble with 25 energy remaining.
Preselecting Bubble before the cave and replaying the old route fails instead:
weapon choice changes the route's interactions, so compatibility cannot be
inferred from a stronger inventory alone.

The exact repaired neutral intro lasts 746 frames. Six consecutive neutral
120-frame actions leave it in phase1, and 26 more frames enter phase2. One
generated suffix permits at most six actions of at most 120 frames each.
The observed idle endpoints share the same archive key and resources;
cheapest replacement rejects the longer idle representatives. The intro-rooted
probe retains only six states without reaching active combat. This diagnoses
a mandatory waiting barrier for the observed same-key idle continuations.
More random budget alone will not preserve those idle prefixes; movement
that changes a retained key may still provide a route around the barrier. The immediate
controller-only workaround explicitly records the wait in the root.

An active HP8/Bubble25 100k probe retains a continuous 20-damage Alien witness
(playerHP2, bossHP8). Its displayed progress watermark is only six damage
because coordinate ordering differs from the archive's combat ranking. Raw
replay confirms ten Bubble hits reducing HP28 to8. Progress summaries alone
would understate this run.

A 36-action jump approach reaches screen27 with full health. A 40-branch
transfer bank reuses the remaining cave route; two branches reach Alien at
HP18/Bubble25. This stronger root waits 748 frames to active phase2. Directly
transferring the HP8 combat suffix to it fails, despite matching visible fight
start fields; the full emulator state still matters. A fresh bounded combat
search from the stronger root is the next test.


## Verified completion and highest-yield follow-ups

The HP18 active-fight probe reaches 22 damage in 100k executions. A 100k
continuation from a retained 22-damage state reaches 24 damage. A new bounded
probe rooted at a surviving 24-damage state clears Alien in 8,322 executions
(934,124 emulated search frames). All roots are composed into one controller
tape; no snapshot restore or RAM edit occurs during its final verification.
The final 28-action searched suffix kills Alien. Another 120 explicit neutral
120-frame actions carry the tape through the ending.

The final input SHA256 is
`95cb39ef895e6e8b09cb170cbb4bd0783a62aa18cb8f090151f859f1cffebd90`.
Raw replay records the winning boss phase at action 6677 and stage14 at 6696.
Ending code subsequently reuses the stage byte; the final decoded value 5 is
not a return to active gameplay. Both backends retain HP6/lives2. The 337.15-second
film starts at the final cave, shows the final blast around 95–98 seconds, the Wily
reveal around 126–130 seconds, and credits from roughly 150 seconds, ending on repeated
“PRESENTED BY CAPCOM U.S.A.” cards. No literal “THE END” card was required or
observed. Reproduction instructions and independent reports are in the
[completion exports](exports/mm2-completion-20260919/README.md).

Ranked permanent improvements supported by this investigation:

1. **Validate encounter state and irreversible progress before tuning search.**
   Teleport stage borrowing, refight completion, Machine shell break/refill,
   and persistent death state changed what the archive could recognize.
   Keep these mechanical distinctions in the adapter and verify lifecycle
   transitions against film and raw traces.
2. **Preserve resources that enable the next transition.** Total energy hides
   Crash/Bubble identity. Full-health, full-Crash entry broke the Machine wall;
   Bubble was necessary for Alien. Test bounded resource alternatives at equal
   memory and physical work under existing [#271](https://github.com/pH14/harmony/issues/271)
   and [#286](https://github.com/pH14/harmony/issues/286). This completion does
   not by itself validate a generic Pareto-retention policy.
3. **Make mandatory waits reachable and replayable.** The 746/748-frame intro
   exceeds one ordinary suffix's 720-frame maximum. Test duration-aware waits
   or longer bursts under [#361](https://github.com/pH14/harmony/issues/361),
   with every emulated frame charged and recorded.
4. **Spend local budget on verified surviving continuations.** Short rooted
   probes, small controller banks, and explicit route repair finished the last
   fights. An improved inventory did not guarantee successful suffix transfer;
   execute the transfer and inspect the arrival. Test a general mechanism
   against matched roots rather than attributing these manual choices to the
   unchanged selector.
5. **Report meaningful milestones separately from coordinate maxima.** The
   displayed Wily6 champion missed an existing boss-entry witness, and a
   watermark understated an archive's 20-damage fight. Preserve first-event
   tapes and best surviving encounter states alongside the spatial champion.

These priorities are evidence-informed next experiments, not five isolated
causal ablations. The result uses historical groundwork, rooted search,
controller banks, manually selected roots, and ordinary deaths/Continue.
It proves continuous completion and identifies productive interventions;
it does not establish an autonomous fresh-game win rate. Source validation
before the final artifact-only changes included 148 NES library tests,
35 MM2 tests, formatting, diff checks, and qualified replay checks.
