<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Super Mario Bros. workload

## Boot

The target idles 39 frames from power-on, presses Start for one frame and
waits for play, which begins at frame 192. A Start press after 33 to 39 idle
frames reaches play at that frame; a later press waits for the next 21-frame
boundary of the title screen. The game's emulator identity names the boot
(`boot=start-after-39-idle-frames`), so tapes recorded after another boot are
rejected.

## Actions

An action is a controller chord held for 2–12 or 96–120 frames. A new chord
changes one control of the chord before it, through the shared NES chord draw
in the [package README](../../README.md#chord-draw). SMB has no tap button.
Holding B to run while the jump button changes is the common SMB move, and a
draw of whole new chords rarely keeps it. The first chord of a new game has
nothing before it and is drawn from the controller vocabulary.

An action runs its chord in one machine run and reads each frame's work RAM
from the run. The target stops reading at the first frame where Mario dies, the
game is won or the state is doomed, and counts the frames up to that one. Its
execution work counts every frame the run produced, as `NesTarget` does. A film of the action keeps
the video and audio of those frames only. The machine has run the
rest of the chord by then, so the target keeps the action's starting state. A
dead or won target takes no further action. A snapshot of it first reruns the
chord from that starting state up to the stop frame, so the snapshot holds the
state Mario died or won in. A viability probe runs its frames the same way and
then returns the machine to the state it started from.

A chord with a hold of 0 waits for control: it holds its buttons, a chord of
120 frames at a time, until the frame after which the player has control
(`$0770` = 1, `$0772` = 3, `$0E` = 8), Mario dies, the game is won, or 2,400
frames pass. The campaign never draws one. Shrink tapes use it at each level
end, so that a level finished a few frames sooner still starts the next level's
inputs at the frame the game hands back control. The target counts each
action's frames and the frames of it spent inside a level, where the game is
in play and `$0E` is not 0, 4, 5 or 7.

A state is doomed when Mario has fallen a block below the ground line over
open floor with no lift in play: the player has control (`$0E` = 8), he is
jumping or falling (`$1D`), not swimming (`$0704`), on the play-area page
(`$B5` = 1), his height `$CE` is at least one 16-pixel block below the
standing height on the floor rows (`$B0`), he is not rising (`$9F`), the two
floor rows of the block buffer (rows 11 and 12 at `$0500` and `$05D0`) under
both his foot columns hold no metatile his feet land on, and no active enemy
slot (`$0F` to `$13`, IDs at `$16` to `$1A`) holds a lift (`$24` to `$2C`)
or a jump spring (`$32`). The game lands his feet on any metatile but blank,
the vine blank `$26`, the coins `$C2` and `$C3` and the hidden blocks `$5F`
and `$60`, and it still lands feet a few pixels into a ledge, which is why
the depth is a full block. Lifts move through pits and catch a falling
Mario below the ground line. A doomed state, under the game's route as well,
ends the job like a death and is never kept.

A job's length comes from its parent's earlier jobs. A parent's first job runs
one chord, and each job that keeps no state and stays in the parent's place
doubles the next one, up to 64 chords. SMB has stretches where the key does not
change: the flag tally lasts about 257 frames and the castle's Toad scene 378.
A job of one chord, at most 120 frames, never crosses them; a parent in front
of one reaches a long enough job after a few doublings.

## Archive key

The progress tier is the route plan's stage and objective (see
[Route plans](#route-plans)). On every level stage the objective is the level's
progress in bands of 64 sixteen-pixel columns. The searcher gives each tier an
eighth of the draws of the tier ahead of it, so the band must not fall along the
route through a level. The camera column restarts in each area, and in 8-4 the route passes
through an underwater area after the castle's third band. A state therefore
keeps the highest band its parent held in the same level. A pipe that sends
Mario back within the same area, as 8-4's wrong pipes do, drops that inherited
band, so those states rank by their own column, and the room restarts at the
page they land on. A state's key is folded over every frame of its path, so the
band and room it inherits come from the frame before it, not from the last
keyed candidate.

The place is the room Mario entered the level section through, his column
(the camera's column plus his column on the screen), his height bucket, whether
every loop check so far went the right way, and the hundreds digit of the game
clock. The holder identity is his column on the screen. The game clock is the
preference, so a faster arrival at a slot displaces the slower holder.

Some castle levels check Mario's height at fixed pages and send him back when
he takes the wrong path. In 7-4 the game
counts the checks passed at `$06DA` and the checks passed on the right path at
`$06D9`; after the third check it sends Mario back unless both are three. A
state with the counts equal can still clear the loop, and one with them unequal
cannot, while both reach the same columns and heights. The key holds the
equality in the place, so a slower state that can still clear the loop keeps
its own slot beside a faster state that cannot. In 4-4 and 8-4 the game leaves
both counters at zero and sends Mario back at a wrong check at once.

The game writes the next level's world and level numbers (`$075F`, `$075C`)
before it loads that level's area. At a warp pipe it writes them as Mario
enters the pipe, about 50 frames before the area loads. After a castle it
writes them on the first frame of area loading (`$0770` = 1, `$0772` = 0). In
both cases the scroll still holds the old level's position, so the old level's
end would read as deep progress in the new level and outrank real play there.
A state whose level numbers differ from the previous state's while its area
(`$074E`, `$074F`) is unchanged keeps the previous state's world and level.
States during area loading have progress 0 and screen column 0, because the
scroll or Mario's position still belongs to the old area.

Camera and player coordinates used by the key are signed 16-bit positions.
Negative positions at the left boundary clamp to zero before conversion to
columns, so an underflow cannot create a false deepest band.

## Route plans

A route is a `search::plan` list of stages in `smb::plan`, chosen with
`SmbGame::with_route` and named in the key and doomed policies. The default
route, `all_levels`, has one stage per level in order. A staged plan that used
the column as each level's objective kept the front two or three columns as the
top tiers, which took 77% to 97% of the draws, and seven of 16 seeds spent 32M
frames in 1-3; the band keeps 64 columns in one tier.

The `warps_1_2_to_4_vine_area_4_2_to_8` route follows the warp zones: 1-1; 1-2 to
its warp zone (WarpZoneControl, `$06D6`, = 4) and the pipe to 4-1; 4-1; 4-2 to
the vine, to the vine area's warp zone (`$06D6` = 6) and the pipe to 8-1; 8-1,
8-2, 8-3; 8-4 until the ending; and the ending. Warp-zone stages use the band as
their objective and pipe stages have one tier.

4-2's underground area (room (2,1)) holds a brick with the vine at column 64,
row 3, above hidden coin blocks at (63,6), (64,5), (65,6) and (66,7). The vine
stage's objective is a ladder of what the player has done there: 1 once he is
at x 960, plus one for each hidden block hit (its metatile in the block buffer
is no longer `$5F`, read while the camera is between x 784 and 1040, where
those pages are loaded), 6 once the vine has spawned (an enemy slot holds the
vine, ID `$2F`, or, with the player at x 960 or more, VineStart_Y `$039D` is
nonzero, which stays set after the vine leaves the screen), 7 while he climbs
(`$1D` = 3), and 8 in the vine area (room (1,15)); 4-2's other rooms, such as
its bonus room and its normal exit, count 0. After the vine spawns, the pipe at x 1348
also leads to the vine area's warp zone while AreaPointer (`$0750`) still holds
the vine area's pointer `$2F`; later enemy data overwrites it with the normal
exit's. This is the route of tool-assisted runs, so the stage is done either way.
Once the vine has spawned no objective step leads toward that pipe, only
climbing (7) does, so a search from the spawn finds the climb first; a strategy
file for this game should give the pipe its own step.

Under the warp route a state is doomed when the world and level numbers name a
level the route omits, or when the camera in 4-2's underground area has passed
x 1040 while the player is not climbing, unless the vine has spawned and
AreaPointer still holds `$2F`, since the brick is then left of the screen. `smb-route <tape> --route <identifier>` replays a tape and prints each
stage it visits with its objectives and the number of doomed actions.

A campaign in the searcher's minimize mode (see
[Minimizing](../../../../dissonance/searcher/README.md#minimizing)) uses the
shrink's per-action cost and lower bound (see [Shrink](#shrink)) to keep the
cheapest goal tape and drop every state whose cost so far plus that bound
reaches it.

## Snapshots

A native snapshot stores the QuickNES state as an LZ4 block with its length in
front, through the shared path in the
[package README](../../README.md#snapshots). The 12,912-byte state compresses
about seven times, and the archive
charges the compressed length, so a memory budget keeps about seven times as
many snapshots resident. A job whose parent snapshot was evicted replays from
the nearest keyframe, and those replayed frames count against the frame budget,
so a small budget searches faster with compressed snapshots. Compression takes
about 8 µs and decompression about 5 µs, against about 105 µs for one emulated
frame.

A snapshot also keeps the last frame's count, decoded state, milestones and
death flag. A restored target needs work RAM for its archive key, its victory
check and the next action's starting scroll position. QuickNES serves reads
right after a restore, so a native snapshot keeps no copy of work RAM and a
restore reads it from the machine. The Consonance backend serves reads only
after a run, so a Consonance snapshot keeps a 2 KiB copy, and the archive
charges it. A compressed native snapshot stores about 1.8 KB.

## Shrink

`smb::shrink` holds the shrink hooks that `SmbGame`'s campaign traits and its
`ShrinkPolicy` call for `search::shrink`. The goal is the
run's terminal predicate, the ending for a speedrun. The cost is frames, then
frames inside a level. A simpler action is the same chord held one frame less,
and held half as long when the hold is above 3; a wait for control has none.
The lower bound is the distance left to the level's exit over the top running
speed of 2.5 pixels per frame, less a 16-pixel margin, counted in the final area
of each level on the warp route (1-1, 1-2, 4-1, 4-2 and world 8), and zero
elsewhere. The stages are the route plan's: a state's stage is the plan stage
its archive key locates, and the run for stage `n` has the terminal predicate
`stage:<route>:<n>`, reached at the first state with control once the plan
locates stage `n` or a later one. Control is part of the goal because two lines
can enter a stage's room with different cutscenes ahead of them: climbing the
4-2 vine can enter the vine area earlier than taking the pipe does and then
gives control about 400 frames later. Stage runs stop at the run's own
terminal: the plan stage whose level a `control_in` terminal names, the stage
after the level a level transition leaves, a stage predicate's own stage, and
for game victory the ending stage, or, on a route without one such as
`all_levels`, the position past its last stage, so that 8-4 has a stage run. The stage there has no stage run, so the last
segment's goal is the run's own terminal.
Under a stage predicate whose stage lies in the current level the lower bound
is zero, since the exit bound measures distance to the level's end and not to
the stage. `search::shrink` therefore shrinks a tape one plan stage at a time.

A level ends on a 21-frame boundary, and the random number generator and the
frame counter carry into the next level, where some enemies read them. So a
saving inside one boundary keeps every later level's inputs valid once each
level end is a wait for control, and a saving that crosses a boundary changes
the generator in every later level. `with_level_end_waits` rewrites a tape so
that each stretch without control that ends a level is one wait for control
holding the buttons of its last frame, and checks by replay that the tape still
ends at the same frame. The second part of the cost lets savings inside one
boundary be accepted until they add up to a whole boundary. Frame counts from
power-on to a level's control therefore move in steps of 21 frames: a saving
inside a level shows in the result only once it adds up to a whole boundary.

`smb-shrink --input <tape.json> --out <dir>` rewrites the tape, runs
`search::shrink` on it from power-on and writes `root-input.json`,
`incumbent-input.json` (the rewritten tape), `best-input.json` after each
improvement, `improvements.jsonl`, `progress.jsonl` (every `--progress-every`
jobs) and `outcome.json`, with frames counted from power-on.
`--terminal control_in:<world>,<level>` makes the goal the first frame with
control in that level (zero-based numbers), and cuts the tape there, so one
level can be shrunk to the next level's start. A tape that never reaches the
terminal is kept whole: its states up to its end or Mario's death seed the
search, `outcome.json` records no initial frames, and its stop is
`no_arrival` if the budget ends before an arrival. `--root-at
control_in:<world>,<level>` sets the shrink's root to the tape's first frame
with control in that level, so only the actions after it change. `--route
<identifier>` keys the archive by that route plan. `--workers`, `--window`,
`--job-budget`, `--work-budget` (emulated frames, including the tape's own
replay), `--ordinary-length` (default 64), `--memory-mib` (default 8192) and
`--stream <path>`, which keeps the campaign stream for replay, set the
campaign.

A rooted game (`SmbGame::with_root`) replays a prefix tape after the boot and
takes that state as its genesis, and its emulator identity names the prefix's
digest. `nes-eval` builds one from `root_input` and takes `smb_terminal` as the
goal, so a search can find one level again from a new prefix. `smb-splice
<prefix.json> <full.json> control_in:<world>,<level> <out.json>` replaces the
full tape's actions up to control in that level with the prefix's, writes the
result, and reports whether it still reaches the ending. `smb-route` reports the
first frame with control in each level beside the level's start frame, and the
room, x position and frame of the last state with control in it.

`smb-window --input <tape.json> --out <dir> --window <actions>` runs the window
search (see the [searcher README](../../../../dissonance/searcher/README.md#window-search))
over the tape's last `<actions>` actions before the goal, with the same
`--terminal`, `--root-at` and `--route` flags as `smb-shrink`. Its actions are
each vocabulary chord held one frame and a wait for control, and its states
merge by `Target::fingerprint`: a hash of the bytes that set Mario's motion,
the level, the area and the timers that decide when he moves next, and the
jump and run bits of the last frame's buttons. `--expansion-budget` and
`--frontier-limit` (default 100,000) bound it. It writes `best-input.json`,
`improvements.jsonl`, `progress.jsonl` and `outcome.json` with frames from
power-on and the lower bound the search has proved.

`smb-fm2-stages <movie.fm2> [shift]` replays an FCEUX movie's first-controller
inputs one frame per line, from power-on, dropping the first `shift` lines or
adding idle frames for a positive one, and prints each level's start, control
and last-control frames and the victory frame, for comparing a tape's levels
with a reference movie.
