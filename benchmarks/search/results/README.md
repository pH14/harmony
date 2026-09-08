<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Recorded search evidence

Keep evidence from each frozen panel immutable. The compact records here contain
seed-level comparisons, registered manifests, source/executable/core/ROM hashes,
host allocation, and hashes of the complete allowlisted exports. Private ROMs,
emulator cores, snapshots, and full campaign streams are not checked in.

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
