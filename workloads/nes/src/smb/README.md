<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Super Mario Bros. workload

## Actions

An action is a controller chord held for 2–12 or 96–120 frames. A new chord
changes one control of the chord before it, through the shared NES chord draw
in the [package README](../../README.md#chord-draw). SMB has no tap button.
Holding B to run while the jump button changes is the common SMB move, and a
draw of whole new chords rarely keeps it. The first chord of a new game has
nothing before it and is drawn from the controller vocabulary.

An action runs its chord in one machine run and reads each frame's work RAM
from the run. The target stops reading at the frame where Mario dies or the
game is won, and counts the frames up to that one. A film of the action keeps
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
Mario below the ground line. The check reads only the action's last frame. A
doomed state ends the job like a death and is never kept.

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
band, so those states rank by their own column.

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

The `warps_1_2_to_4_vine_4_2_to_8` route follows the warp zones: 1-1; 1-2 to
its warp zone (WarpZoneControl, `$06D6`, = 4) and the pipe to 4-1; 4-1; 4-2 to
the vine, to the vine area's warp zone (`$06D6` = 6) and the pipe to 8-1; 8-1,
8-2, 8-3; 8-4 until the ending; and the ending. Warp-zone stages use the band as
their objective and pipe stages have one tier.

4-2's underground area (room (2,1)) holds a brick with the vine at column 64,
row 3, above hidden coin blocks at (63,6), (64,5), (65,6) and (66,7). The vine
stage's objective is a ladder of what the player has done there: 1 once he is
at x 960, plus one for each hidden block hit (its metatile in the block buffer
is no longer `$5F`, read while the camera is between x 784 and 1040, where
those pages are loaded), 6 once an enemy slot holds the vine (ID `$2F`), 7 while
he climbs (`$1D` = 3), and 8 in the vine area. The route climbs the vine
rather than the pipe clip of tool-assisted runs, which has no RAM signal before
the warp.

Under the warp route a state is doomed when the world and level numbers name a
level the route omits, or when the camera in 4-2's underground area has passed
x 1040 while the player is not climbing, since the brick is then left of the
screen. `smb-route <tape> --route <identifier>` replays a tape and prints each
stage it visits with its objectives and the number of doomed actions.

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

`smb::shrink` declares the `ShrinkPolicy` for `search::shrink`. The goal is the
run's terminal predicate, the ending for a speedrun. The cost is frames, then
frames inside a level. A simpler action is the same chord held one frame less,
and held half as long when the hold is above 3; a wait for control has none.
The lower bound is the distance left to the exit of a world-8 level over the
top running speed of 2.5 pixels per frame, less a 16-pixel margin, counted only
in the level's final area past the point where that area's route begins, and
zero elsewhere.

A level ends on a 21-frame boundary, and the random number generator and the
frame counter carry into the next level, where some enemies read them. So a
saving inside one boundary keeps every later level's inputs valid once each
level end is a wait for control, and a saving that crosses a boundary changes
the generator in every later level. `with_level_end_waits` rewrites a tape so
that each stretch without control that ends a level is one wait for control
holding the buttons of its last frame, and checks by replay that the tape still
ends at the same frame. The second part of the cost lets savings inside one
boundary be accepted until they add up to a whole boundary.

`smb-shrink --input <tape.json> --out <dir>` rewrites the tape, shrinks it and
writes `incumbent-input.json`, `best-input.json` after each improvement,
`improvements.jsonl`, `progress.jsonl` and `outcome.json`, with frames counted
from power-on.
