<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Super Mario Bros on the consolidated archive, ms02, 2026-09-22

The three SMB regression cells (`smb-regression-three.json`: power-on,
seeds 20260905 to 20260907, 24 workers, 256 MiB, two million executions)
on the old build and on the twelfth build of the searcher consolidation
branch. Old build: `builds/p3-recency17` (a77fbd7b772d, commit 09a8bb60b,
run `runs/smb23-recency17-20260921`). New build: `builds/consol12`
(31b675b5ee33, commit 4b16ae834, run `runs/smb-consol12-20260922`). Both
solve every cell. Executions are from the cell result; level arrivals are
the first progress record of each cell that reports the level.

| seed | build | victory | reached 8-1 | left 8-1 | rate (exec/s) |
|---|---|---|---|---|---|
| 20260905 | old | 1,430,641 | 514,800 | 1,280,300 | 1,586 |
| 20260905 | new | 1,058,642 | 715,100 | 772,600 | 1,415 |
| 20260906 | old | 1,516,089 | 657,500 | 1,418,100 | 1,581 |
| 20260906 | new | 1,140,206 | 659,700 | 761,300 | 1,611 |
| 20260907 | old | 850,277 | 499,300 | 766,200 | 1,617 |
| 20260907 | new | 1,346,570 | 1,045,200 | 1,101,000 | 1,546 |

The new build takes 56 to 102 thousand executions to cross 8-1 where the
old build takes 267 to 766 thousand. The new build is slower before world
2 (its first level ends at 8 to 18 thousand executions against 1 to 2
thousand) and in the 7-4 maze on seed 20260907 (347 thousand executions
against 23 thousand).

Eleven earlier builds of the branch failed these cells; the plan log in
`docs/SEARCHER-CONSOLIDATION-PLAN.md` names each loss and fix. The
decisive ones, read from the fourth and tenth builds' 8-1 draw tables and
a film of the tenth build's deepest 8-1 tape: the deepest tapes reached
the last two screens of 8-1 with the clock at one and died to the timer,
and the game-clock preference added to fix that could not displace a
slower holder because the holder identity was a six-bit hash of the whole
RAM. The SMB key's tier is now (world, level, four-screen band) ranked two
to one per band, its preference is the game clock, and its holder identity
is the screen position.
