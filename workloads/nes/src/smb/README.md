<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Super Mario Bros. workload

## Actions

An action is a controller chord held for 2–12 or 96–120 frames. A new chord
changes one control of the chord before it. The controls are the D-pad, with
nine positions, and the A and B buttons. The draw picks the D-pad, A, B or
no change with equal odds. A D-pad change moves to one of the other eight
positions, and an A or B change toggles that button. Holding B to run while
the jump button changes is the common SMB move, and a draw of whole new chords
rarely keeps it. The first chord of a new game has nothing before it and is
drawn from the controller vocabulary.

A job's length comes from its parent's earlier jobs, through the searcher's
`one_doubling_while_in_place_up_to_64` suffix shape. A parent's first job runs
one chord, and each job that keeps no state and stays in the parent's place
doubles the next one, up to 64 chords. SMB has stretches where the key does not
change: the flag tally lasts about 257 frames and the castle's Toad scene 378.
A job of one chord, at most 120 frames, never crosses them; a parent in front
of one reaches a long enough job after a few doublings.

## Archive key

The progress tier is the world, the level and the level's progress in bands of
64 sixteen-pixel columns. The searcher gives each tier half the draws of the
tier ahead of it.

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

## Snapshots

A native snapshot stores the QuickNES state as an LZ4 block with its length in
front. The 12,912-byte state compresses about seven times, and the archive
charges the compressed length, so a memory budget keeps about seven times as
many snapshots resident. A job whose parent snapshot was evicted replays from
the nearest keyframe, and those replayed frames count against the frame budget,
so a small budget searches faster with compressed snapshots. Compression takes
about 8 µs and decompression about 5 µs, against about 105 µs for one emulated
frame.

A snapshot also keeps the last frame's count, decoded state, milestones and
death flag. It leaves out the list of changed RAM addresses and the log line,
which no consumer reads after a restore; a restored target reports no changed
addresses, as a freshly booted one does. That cuts the stored size of a
compressed snapshot from about 2.9 KB to about 1.8 KB.
