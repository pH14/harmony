<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Draws on Kraid's hideout from the bombs pickup, ms02, 2026-09-19

Follows kraid-acceptance-lineage-20260919.md, whose control and area-depth
arms are the first two rows of every table here. Every arm is rooted at seed
3's bombs pickup at Brinstar (25,5) (32.7 energy, 0 missiles, capacity 5,
one tank, morph ball, long beam, bombs), seeds 11 to 13, 1M executions,
4 workers, 6144 MiB, selector
hierarchy_uniform_128_energy_frontier_cheapest:64,6,12,2,16. Milestone
tables: kraid-bombs-*-20260919-milestones.json beside this file.

## How the band rank is weighted

`draw_group_index` in the searcher weights a group of a class as
`(energy << 16) >> rank`, where rank counts the frontier bands strictly
ahead of the group under `progress_cmp` and energy is `256 >> (barren
draws / scale)`. A level of progress is worth a factor of two; barren decay
divides by up to 256. The class order is the only rank that moves draws
wholesale.

## Brinstar and Norfair as peers, hideouts one level above (72354ff8)

| arm | seed | hideout | ice beam | hideout cells | max missiles | share after entry |
|---|---|---|---|---|---|---|
| control 9e05c6d0 | 11 | 324,028 | 744,464 | 17 | 5 | 4.9% |
| control | 12 | 349,766 | 724,554 | 19 | 4 | 3.4% |
| control | 13 | 295,093 | never | 31 | 5 | 5.8% |
| area depth e6a72b03 | 11 | 324,028 | 644,526 | 44 | 10 | 32.3% |
| area depth | 12 | 525,010 | never | 31 | 5 | 13.2% |
| area depth | 13 | 513,908 | never | 29 | 5 | 8.8% |
| peers 72354ff8 | 11 | 324,028 | 765,951 | 21 | 5 | 7.6% |
| peers | 12 | 349,766 | 577,676 | 29 | 8 | 6.4% |
| peers | 13 | 295,093 | never | 14 | 4 | 5.8% |

No arm enters Kraid's room (8,29). The peers arm enters the hideout at the
control's exact executions on every seed and keeps the ice beam on the two
seeds where the control takes it, so the delay of the area-depth arm was the
Norfair rank. Its hideout share after entry is one level's worth above the
control, as the weight formula predicts, and no seed reaches (8,28) or the
row of Kraid's room. Under its preregistration (bombs-peers-prereg.md on
ms02) the peers rank is not accepted: it costs nothing and reaches nothing.

Seed 12's hideout arrival tape (520 actions) reaches (7,19) with 0 missiles
and 1 energy. FILM REPORT PENDING.

## Area depth ahead of items in the class order (34786e39)

RESULT PENDING.
