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
