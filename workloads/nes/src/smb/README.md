<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Super Mario Bros. workload

## Archive key

The progress tier is the world, the level and the level's progress in bands of
64 sixteen-pixel columns. The searcher gives each tier half the draws of the
tier ahead of it.

The place is the room Mario entered the level section through, his column
(the camera's column plus his column on the screen), his height bucket, whether
every loop check so far went the right way, and the hundreds digit of the game
clock. The holder identity is his column on the screen. The game clock is the
preference, so a faster arrival at a slot displaces the slower holder.

Castle levels that loop check Mario's height at fixed pages. In 7-4 the game
counts the checks passed at `$06DA` and the checks passed on the right path at
`$06D9`; after the third check it sends Mario back unless both are three. A
state with the counts equal can still clear the loop, and one with them unequal
cannot, while both reach the same columns and heights. The key holds the
equality in the place, so a slower state that can still clear the loop keeps
its own slot beside a faster state that cannot. In 4-4 and 8-4 the game leaves
both counters at zero and sends Mario back at a wrong check at once.
