<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Recorded search evidence

Keep evidence from each frozen panel immutable. The compact records here contain
seed-level comparisons, registered manifests, source/executable/core/ROM hashes,
host allocation, and hashes of the complete allowlisted exports. Private ROMs,
emulator cores, snapshots, and full campaign streams are not checked in.
Disk figures describe the per-cell output directory, excluding shared assets,
build bundles, and private runtime temporary files outside it. Process I/O
measurements are separate; the figures are not total filesystem occupancy.
[`build-provenance.json`](build-provenance.json) maps frozen builds 003–005 to
Git commits by exact source-hash equality against freshly extracted Git trees.
Build 005 is `02463cae`. Its executable and original source-copy metadata remain
immutable; later commits are not retroactively attributed to that measured build.

## Historical depth audit

[`progress-audit-007.json`](progress-audit-007.json) corrects the scope of the
005 comparison: historical deep-search reach was **not qualified**. Seventeen
retained tapes replay twice with the current decoder: ten panel champions,
six historical Metroid inputs, and the Mega Man 2 power-on prefix reaching Wily
4 with all eight weapons. Historical Metroid champions reach Kraid's area with
Bombs and another beam; the ten panel champions reach Norfair with Morph Ball.
Ridley-area entry remains supported by old aggregate records, not a recovered
champion tape. No recovered Metroid tape verifies a boss defeat.

The audit separates unknown old fields from unobserved new milestones, inventory
from boss counts, missile capacity from energy tanks, and branch unions from
individual trajectories. It also records a 5,000-job reporting qualification:
all parent selections, mutation seeds, work, and admission decisions match build
005; only the declared policy/observation versions and result digests differ.
Both the full campaign and each named discovery witness replay successfully.
The Ridley bit correction is independently covered by positive/negative decoder
tests; the matched small run does not exercise a boss defeat.

## Fresh full validation

[`full-validation-005.json`](full-validation-005.json) retains all 190 cells in
the two preregistered 95-cell panels, using seeds 20260920–20260924. There are no
infrastructure errors. Every claimed victory has a twice-replayed native
witness. Both arms solve 65 cells, but the successes differ: continuations gain
Nova level 9 seed 20260924 and level 25 seed 20260922, while losing SMB seed
20260923 and Crash Man seed 20260920. These heterogeneous cells are not
interchangeable trials of one task.

| Origin | Control solves | Continuation solves | Control median victory frames | Continuation median victory frames |
| --- | ---: | ---: | ---: | ---: |
| metroid-full | 0/5 | 0/5 | — | — |
| mm2-air | 5/5 | 5/5 | 10,025,392 | 6,697,837 |
| mm2-bubble | 5/5 | 5/5 | 3,495,044 | 2,459,496 |
| mm2-crash | 5/5 | 4/5 | 30,620,944 | 3,181,997 |
| mm2-flash | 5/5 | 5/5 | 2,547,399 | 1,585,123 |
| mm2-heat | 0/5 | 0/5 | — | — |
| mm2-metal | 5/5 | 5/5 | 2,383,463 | 2,073,583 |
| mm2-quick | 5/5 | 5/5 | 3,010,345 | 3,553,894 |
| mm2-wood | 0/5 | 0/5 | — | — |
| nova-full | 0/5 | 0/5 | — | — |
| nova-level-1 | 5/5 | 5/5 | 384,482 | 607,444 |
| nova-level-17 | 5/5 | 5/5 | 333,563 | 515,894 |
| nova-level-25 | 0/5 | 1/5 | — | 4,798,059 |
| nova-level-33 | 5/5 | 5/5 | 317,384 | 362,082 |
| nova-level-9 | 0/5 | 1/5 | — | 2,898,142 |
| smb-full | 5/5 | 4/5 | 42,340,665 | 39,256,001 |
| stb-easy | 5/5 | 5/5 | 57,016 | 53,389 |
| stb-fair | 5/5 | 5/5 | 516,467 | 358,408 |
| stb-hard | 5/5 | 5/5 | 934,147 | 810,191 |

Victory-cost medians include successes only. In particular, Crash Man's much
lower conditional median does not erase its lost pass, and SMB's medians use
different successful seed sets. The record includes paired ratios restricted to
seeds solved in both arms, gained/lost solves, stop reasons, resource summaries,
and every seed's observations. A single new Nova fixture success is not a
qualified reliable recipe. Prior clear flags in later-level Nova fixtures are
setup state, not levels solved during that search.

Metroid remains at one item, missile capacity 10 and two combined tanks in both
arms. At the last observation at or below 50 million frames, median observed
map coverage is 60 cells in both arms. Whole-game Nova remains at seven cleared
flags on every seed; Heat Man and Wood Man remain unsolved. These plateaus are
retained as future searcher challenges, without adding route hints to adapters.

The full panels run three concurrent eight-worker jobs. Only 40/95 pairs have
the same recorded CPU allocation; use their admitted frame costs for search
quality, and the isolated experiment below for precise throughput claims.
Per-seed frames/s, executions/s, OS RSS, logical memory, output disk, I/O and
phase times remain available. Peak process RSS across the full panels is at
most 2,701 MiB; the largest cell output footprint is under 12 MiB. These output
figures exclude shared assets/builds and runtime files elsewhere.

Adopt the qualified two-result native execution profile and retain the
count-weighted SMB reference below. Keep continuation replay and the other
selection policies as explicit experiments: the fresh-panel tradeoffs do not
support a universal default promotion. The zero-dispatch SMB regression is
tracked in [#275](https://github.com/pH14/harmony/issues/275). These seeds are now
observed reference cases for regression time series; future promotion decisions
need a newly registered unseen seed panel.

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
one result per worker by default. [`qualification-005.json`](qualification-005.json)
records full native campaign replay qualification at all 19 origins under the
two-slot profile, with ceilings of 500 executions and 50,000 frames. These small
runs verify full campaign/report/checkpoint replay plus two fresh witness
replays; they do not require each game to be solved. Generic fixtures separately
verify stream/checkpoint equality under memory pressure and frame caps. Quality
panels use the qualified overlap configuration for both control and candidate.

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

[`progress-continuation-005.json`](progress-continuation-005.json) isolates
semantic progress ordering on top of count-weighted continuation replay.
At the last observation at or below 50 million admitted frames, Metroid covers
69/77/72 map cells versus 60/60/61 (median 72 versus 60, +20%). No run gains
another item or missile-capacity tier. At termination the counts are 75/83/75,
but those runs also performed more frames, so the matched-work comparison is
the stronger evidence. These are three development seeds and sampled counts.

A supplemental review in [`metroid-diagnostics-005.json`](metroid-diagnostics-005.json)
also records a verified three-tank witness on semantic seed 20260906, versus
two tanks in the other pilot runs. The combined missile/energy-tank milestone
improves even though the primary equipment/missile-capacity watermark does not.
That third tank is already observed below 50 million frames. Area values in
these reports are bitsets. This interpretation does not change the frozen
full-panel candidate.

Metal Man still clears on every seed, but its median cost rises from 1,889,540
to 5,172,804 frames (+173.8%). The other workload policies retain their default
progress ordering and reproduce their prior frame costs. Semantic progress is
an explicit exploration experiment, not the general recommended selector.

[`keycount-continuation-005.json`](keycount-continuation-005.json) completes
the pilot matrix. Relative to continuation alone, retained-key weighting raises
median frame cost by 12.6% on SMB, 57.4% on Metal Man, 72.5% on Nova level 1,
and 27.4% on STB Hard. Metroid stays at the same item/capacity plateau and
observes 60/61/63 map cells. Whole-game Nova reaches seven clears on all seeds.
The record also compares directly with entry counts: key history recovers that
variant’s one six-clear Nova trial, but still does not beat continuation alone
as the general candidate.

The full candidate registration is
[`candidate-registration-005.json`](../candidate-registration-005.json).
It selects continuation replay with the original parent selector before any
completed full-panel outcome was observed, using five seeds disjoint from all
pilot and SMB reference validation seeds. The full comparison retains every
registered origin, budget and failure; no retuning from validation outcomes.

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
