<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Recorded search evidence

Keep evidence from each frozen panel immutable. The compact records here contain
seed-level comparisons, registered manifests, source/executable/core/ROM hashes,
host allocation, and hashes of the complete allowlisted exports. Private ROMs,
emulator cores, snapshots, and full campaign streams are not checked in.

## Bounded physical overlap

[`prefetch-005.json`](prefetch-005.json) compares one versus two unadmitted
result-bearing jobs per physical executor. All 18 pairs have identical recorded
search stream hashes, including frame counts, selections and outcomes. Runs use
24 workers, a two-reservation logical window, 512 MiB, one isolated matrix job,
and the same executable. These are short throughput trials, bounded to at most
50,000 executions and 10 million frames, with smaller ceilings for quick cases.

| Origin | Median frames/s, one slot | Median frames/s, two slots | Median paired improvement |
| --- | ---: | ---: | ---: |
| SMB new game | 282,343 | 401,213 | 42.1% |
| Metroid new game | 366,292 | 514,904 | 40.1% |
| Metal Man stage | 421,640 | 577,055 | 36.9% |
| Nova level 1 | 439,732 | 597,582 | 35.9% |
| Nova whole game | 420,806 | 574,284 | 35.8% |
| STB Hard | 464,264 | 613,718 | 32.2% |

The largest paired increase in OS peak RSS was 3.83 MiB. This native result does
not establish the cost of buffering whole-VM snapshots; the generic API retains
one result per worker by default. The two-slot option passed full native campaign
replay qualification at all 19 origins, plus generic stream/checkpoint equality
under memory pressure and frame caps. Quality panels use the qualified overlap
configuration for both control and candidate.

Whole-game Nova reached 5, 6, and 7 cleared-level flags in these short runs. This
confirms execution continues through intermediate clears, without claiming all
40 levels are solved. Throughput trials do not require whole-game completion.

## Extended SMB count panel

[`smb-count-extended.json`](smb-count-extended.json) retains five paired seeds at
both 256 and 2,048 MiB, with a one-reservation window and the original
400,000-execution / 80-million-frame ceilings. The main-mechanism control solved
3/5 at each memory budget; entry count weighting solved 4/5 at each. Some seeds
regressed, including a frame-capped failure at 256 MiB. The two memory conditions
reuse the same five seeds and are not ten independent seed trials. These results
show a panel gain but do not qualify a recipe that solves every required cell.

## Development panel 002

[`development-002.json`](development-002.json) records two controlled ablations
on the same executable per pair. The integration base is `444ac54c`; the control
uses its search mechanisms with the imported game adapters. These runs precede
the thinner MM2 v18 key and the admitted-frame ceiling, and must not be combined
with those later conditions as if the adapters and budgets were unchanged.

At eight workers and 2,048 MiB, the continuation variant cleared Metal Man on
all three development seeds, as did the control. Median admitted frames to a
verified clear fell from 6,491,257 to 2,447,645 (62.3%). One seed regressed from
2,063,494 to 2,447,645 frames; the other two improved. Nova level 1 and STB Hard
also cleared all three seeds in both variants. SMB cleared two of three in both.
Metroid reached one item and missile capacity 10 in all six runs, without an
ending. Its shorter continuation runs performed fewer frames at the same
execution ceiling; that is not evidence of faster completion or more progress.

The separate 24-worker SMB comparison, at 2,048 MiB and 400,000 executions,
cleared two of three seeds with either selector. Entry count weighting reduced
the successful runs' frame costs, but a different seed failed. This rejects a
claim of improved reliability based on the small panel.

The figures and raw compact exports are retained on ms02 under
`/root/harmony-search-eval/public`. The JSON records include their input hashes.
Concurrent eight-worker runs describe a shared workload mix; use admitted frame
cost for search quality. Their wall timings do not establish isolated emulator
throughput. Three-seed development results do not establish generality or
statistical confidence; later validation must retain the failures and use fresh
seeds.
