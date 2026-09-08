<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Recorded search evidence

Keep evidence from each frozen panel immutable. The compact records here contain
seed-level comparisons, registered manifests, source/executable/core/ROM hashes,
host allocation, and hashes of the complete allowlisted exports. Private ROMs,
emulator cores, snapshots, and full campaign streams are not checked in.
[`build-provenance.json`](build-provenance.json) maps frozen builds 003–005 to
Git commits by exact source-hash equality against freshly extracted Git trees.
Build 005 is `02463cae`; later evidence and documentation commits do not change
its executable. The original source-copy build metadata remains immutable.

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

## Continuation replay with current workload policies

[`continuation-005.json`](continuation-005.json) repeats the complete three-seed
development pilot with the MM2 v18 key and identical two-window/two-slot
execution settings in both arms. Metal Man clears on every seed in both arms;
continuations reduce each seed’s frame cost, and the median falls from
2,108,987 to 1,325,844 frames (37.1%). Nova level 1’s median improves by 26.9%,
with one seed regressing. SMB and STB Hard retain all three passes; STB’s median
frame cost rises by 7.3%, so the gain is not uniform across games.

Metroid retains the same one-item, missile-capacity-10 plateau. Observed map
cells are 58/60/60 for the control and 62/56/62 with continuation replay. Whole-
game Nova remains at seven cleared flags on all seeds. The evidence includes
actual continuation dispatch counts; SMB dispatches none because its policy
reports no strict same-slot preference improvements. These are development
results; the full candidate comparison uses a separate seed panel.

[`count-continuation-005.json`](count-continuation-005.json) then adds entry
counts to continuation replay, keeping the same panel. Relative to continuation
alone, median frame costs change by −9.1% for SMB, +42.5% for Metal Man, +36.8%
for Nova level 1, and +28.9% for STB Hard. All previously solved cases still
pass, but whole-game Nova reaches 7/7/6 clear flags rather than 7/7/7. Metroid
retains its plateau, observing 60/60/61 map cells. This does not support adding
counts to the general candidate; the qualified SMB reference is a separate
resource condition.

## Qualified SMB reference

[`smb-reference-005.json`](smb-reference-005.json) records four development
seeds and five separately registered fresh validation seeds. All nine fresh
whole-game searches produced twice-replayed winning witnesses. The reference
uses entry counts, 24 workers, 2,048 MiB, a two-reservation window, two physical
result slots, and ceilings of 600,000 executions / 120 million admitted frames.
No gameplay tape or adapter change is supplied.

The five validation seeds all passed in 89–251 seconds including verification
(median 143 seconds), with first-victory frame costs of 28.2–107.6 million.
OS peak process RSS ranged from 2,530 to 2,889 MiB despite the 2,048 MiB logical
archive budget; final per-cell disk footprints were 6.1–14.5 MiB.
Seed 20260911 needed 598,013 executions, leaving little headroom under the gate.
The four development seeds took 122–167 seconds. These small fixed panels do
not establish that arbitrary seeds always succeed. Run the checked
[`smb-reference.json`](../smb-reference.json) to repeat this recipe; retain the
stricter stress panel below as a separate condition.

## Extended SMB count panel

[`smb-count-extended.json`](smb-count-extended.json) retains five paired seeds at
both 256 and 2,048 MiB, with a one-reservation window and the original
400,000-execution / 80-million-frame ceilings. The main-mechanism control solved
3/5 at each memory budget; entry count weighting solved 4/5 at each. Some seeds
regressed, including a frame-capped failure at 256 MiB. The two memory conditions
reuse the same five seeds and are not ten independent seed trials. These results
show a panel gain but do not qualify a recipe that solves every required cell.

[`smb-keycount-004.json`](smb-keycount-004.json) compares the same entry-count
panel with count history retained by archive key. Key history solved 3/5 at
256 MiB and 4/5 at 2,048 MiB, versus entry counts’ 4/5 at each. The extra
mechanism does not displace the simpler selector on this evidence.

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
