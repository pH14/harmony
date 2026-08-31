<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# Dissonance campaign scaling experiment

This record covers the `goal/searcher-scaling` experiment on `ms02`. It keeps
the throughput work separate from search-policy tuning and records negative
results as well as the retained scheduler change.

## Protocol

- Host: `ms02`, Intel Core Ultra 9 285HX, 24 physical CPUs (8 performance and
  16 efficiency cores), one hardware thread per core.
- Campaign seed: `6672613057367113729`.
- ROM SHA-256:
  `0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea`.
- Release build, genesis origin, action limit 512, and otherwise identical
  campaign configuration at every worker count.
- Baseline throughput points stop at 2,000 executions or two minutes, whichever
  comes first. The fixed curve runs a complete 30,000 executions at every
  worker count.
- Scaling efficiency is `(N-worker executions/s) / (N * one-worker
  executions/s)`. Frames/s is also shown because admission order changes the
  deterministic search trajectory and therefore the amount of emulator work in
  one execution.

The result artifacts are preserved on `ms02` under
`/root/harmony-searcher-scaling/results`. The baseline curve is in
`before-short`, the instrumented profile is in
`profile-instrumented-before-24x2000`, the fixed 1--16 worker curve is in
`window16-dispatch-curve30k`, and that stage's 24-worker run is in
`window16-dispatch-30k`. The authoritative sole-QuickNES acceptance curve,
repeat, and mature-archive run are together in
`quicknes-final-clean-window64-curve30k`.

## Profile before changing code

The instrumented 24-worker, 2,000-execution run took 60.498 seconds. Summed
worker busy time was 397.966 seconds, or 27.4% utilization across 24 workers;
the measured idle fraction was 72.6%, not greater than 95%. The coordinator
spent 59.704 seconds waiting for the next required worker result.

The entire measured coordinator service cost was 0.534 seconds, 0.267 ms/job:

| Coordinator component | Total | Per completed job |
| --- | ---: | ---: |
| result JSON/digest | 0.3884 s | 0.1942 ms |
| archive admission | 0.0296 s | 0.0148 ms |
| stream append | 0.0163 s | 0.0082 ms |
| selection | 0.0912 s | 0.0456 ms |
| other measured service | 0.0085 s | 0.0043 ms |

The original serial-stage diagnosis was therefore falsified. The bottleneck
was strict reservation-order head-of-line blocking: the coordinator waited for
one named logical worker while already-finished physical workers could not be
reissued.

`perf` attributed more than 85% of CPU samples to TetaNES PPU/frame execution.
State compression accounted for about 0.26%; searcher and coordinator frames
were about 0.01%. No cache or IPC pathology was observed.

## Corrected short baseline

| Workers | Executions | Wall time | Executions/s | Frames/s |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 669 | 120.602 s | 5.547 | 1,207.94 |
| 8 | 1,713 | 121.622 s | 14.085 | 3,173.31 |
| 24 | 2,000 | 60.071 s | 33.294 | 6,815.32 |

The required absolute 24-worker threshold is therefore 166.471 executions/s
(`5 * 33.294185`).

## Coordination change (retired-core stage)

The first deterministic-window policy was
`deterministic_window_16_per_worker_v1`:

- logical lanes select a bounded deterministic window and retain ownership of
  their RNG and recorded worker identity;
- pure job specifications are queued in reservation order;
- any idle physical worker may execute the next specification;
- finished results are buffered by absolute reservation number and admitted in
  that exact order.

Consequently physical completion timing cannot reach the archive, draw tables,
stream bytes, or subsequent selection. At this stage, historical
reservation-order and completion-order streams remained replayable under the
retired target build. The obsolete named-worker reply buffer was removed.

The key behavior-preserving comparison used the same deterministic 30K logical
stream before and after physical redispatch. Before redispatch it took
1,017.324 seconds (29.489 executions/s); after redispatch it took 396.441
seconds (75.673 executions/s), a 2.57x speedup with identical stream, archive,
and snapshot bytes.

## Retired-core 30K scaling curve

| Workers | Wall time | Executions/s | Exec speedup | Exec efficiency | Frames | Frames/s | Frame efficiency | Versus baseline 24w |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 5,480.286 s | 5.474 | 1.000x | 100.0% | 6,782,116 | 1,237.55 | 100.0% | 0.164x |
| 2 | 2,269.862 s | 13.217 | 2.414x | 120.7% | 5,500,848 | 2,423.43 | 97.9% | 0.397x |
| 4 | 1,437.940 s | 20.863 | 3.811x | 95.3% | 6,116,965 | 4,253.98 | 85.9% | 0.627x |
| 8 | 962.397 s | 31.172 | 5.694x | 71.2% | 7,892,402 | 8,200.78 | 82.8% | 0.936x |
| 16 | 482.220 s | 62.212 | 11.365x | 71.0% | 5,973,504 | 12,387.51 | 62.6% | 1.869x |
| 24 | 396.441 s | 75.673 | 13.824x | 57.6% | 7,003,731 | 17,666.50 | 59.5% | 2.273x |

The 24-worker execution-efficiency target is 105.104 executions/s (`24 *
5.474166 * 0.8`). The fixed run reaches 75.673 executions/s, so it misses both
acceptance thresholds:

- 57.6% execution scaling efficiency versus the required 80%;
- 2.273x absolute throughput versus the required 5x.

The complete run also exercises a mature archive. From the last progress sample
at execution 28,088 and 72,922 retained entries to completion at execution
30,000 and 78,331 retained entries, it sustained about 53.0 executions/s over
the final 36.1 seconds. The full-run average is not hiding a collapse caused by
the old serial coordinator stage, although the deeper deterministic trajectory
is slower than the early campaign.

## Retired-core exact replay

Every fixed 30K stream is replayed from genesis with the same build. The live
artifact hashes are:

| Workers | Stream SHA-256 | Exact replay |
| ---: | --- | --- |
| 1 | `5704603890c5220b10893c3377ad92bdd5e23930baae9fb9cfc6b6055bfb53d7` | pass |
| 2 | `663c64de347e2df999a96ee709c3643139d0c215ab9d96ff52a23a45495982dc` | pass |
| 4 | `76c2581557f6fdf3b15dd3e9ca2630e2f2a038fbd75397be63009c407f3e4aa5` | pass |
| 8 | `2f05303a2440dbc91d72cfa10405297d5dfc4f5079f9583d5088400b1d1f9766` | pass |
| 16 | `0dc660747bd58b784528414fc30564720c48b81e9f24bdbf3ff17e849e677f4d` | pass |
| 24 | `9c949fb595e1cde2bfd768be705c84518131ad5a31d6e4a6b17da750fd8281a9` | pass |

All six saved `replay-verdict.json` files report `replay_verified: true` and
30,000 completed executions. For every row, the regenerated archive and snapshot
files match the live SHA-256 byte-for-byte. Their archive/snapshot hashes are:

| Workers | Archive SHA-256 | Snapshots SHA-256 |
| ---: | --- | --- |
| 1 | `b946b63c5951051ed89bda8e6325b3cde3bfd45d289533f5574943c5cff2b8ed` | `492c74ea558c84643a98c3fc8a5011cc5d527ed845066f3cf0cd08ed927d6299` |
| 2 | `24c9819a7c5383117d5a5f1a5caf24805512b4cf3e3bea991d58a9c1c2b080ff` | `5ca76fbf9ea62c6969b8e01ff030fc6caff195c67b1f3504264a0e5043e1f872` |
| 4 | `f6c60f284792a13e095816e133cfe8c1bc4256f64ca605d51cfd2faf50c11917` | `0134bcead97c92964e79393030d8e0bdc19f766f730528dcb0874f7c8fd4739c` |
| 8 | `0ef4016fd9a9041092b8b52ed202f7cf1b438b574f5651719488afab81b47165` | `a2822124410f100fbe30591d4650b782da1fd722553e060a3ea66caba4e5ee29` |
| 16 | `c323e4cdfd7d3aa6bbaad4b4bf98972701e33effce597fdbc9b601c31aa45612` | `3d014dd75d4bd7c5426ba198096c015b573ecbbfa5976c1f05394e6a1706b312` |
| 24 | `305b66685500dc618948e051597f9aa8ab2d2a20a735a14c8bc28d941eff359f` | `1356844fbc6dc1d10e1204e97633b126b9c3d38b0f8b9ac6b499663b012aeec2` |

## Retired-core visible search-behavior change

The corrected protocol stopped the long baseline after interruption and
preserved its latest complete 25K checkpoint rather than rerunning it. This is
the closest mature before/after behavior comparison and is intentionally not a
throughput comparison:

| Counter | Baseline reservation ring, 25K checkpoint | Fixed deterministic window, 30K |
| --- | ---: | ---: |
| progress watermark | world 1, level 0, progress 87 | world 0, level 1, progress 195 |
| max 1-1 scroll bucket | 195 | 196 |
| reached 1-1 flag | true | true |
| reached 1-2 | true | true |
| reached onward | true | false |
| retained | 90,193 | 78,331 |
| rejected | 19,141 | 27,175 |
| deaths | 10,819 | 9,977 |

This difference is an expected consequence of selecting a bounded window before
admitting its results. No search policy or game-specific behavior was tuned to
improve throughput.

## TetaNES fixed-buffer state experiment

The pinned TetaNES core exposes uncompressed fixed-buffer state APIs. A temporary
implementation measured them separately and then used them in the complete
restore--run--observe--snapshot workload:

- compressed state at the sampled point: 302 bytes;
- raw serialized state: 21,518 bytes (71.25x larger);
- compressed save/load: 0.0738/0.0330 ms;
- raw reused-buffer save/load: 0.000812/0.0210 ms;
- compressed 24-worker short campaign: 110.658 executions/s;
- raw 24-worker short campaign: 110.424 executions/s (0.21% slower);
- live snapshot artifact: 20.7 MB compressed versus 125.8 MB raw.

Raw-state exact replay passed, but end-to-end throughput did not improve and
checkpoint storage grew about sixfold. The experiment was removed, so
checkpoint compatibility was unchanged at that stage; those target-specific
checkpoints were later retired by the QuickNES migration.

Other removed negative experiments included direct frame clocks, borrowed WRAM,
snapshot clone movement, fat/ThinLTO, native CPU targeting, panic abort, changed
inlining thresholds, and background-fetch skipping. None met the bar.

## QuickNES migration

QuickNES revision `26bb785c9deddb66a17717b21bb4e328f03ade32` is now the
sole NES execution target. The measured `-O2` libretro shared object has SHA-256
`a6b0876d999a97c518fff3ffd6a60752497a2699160e78ef2b1370787d497eda`.
Video and audio are disabled, system RAM is read directly from the validated
2 KiB libretro block, and snapshot/restore use the core's fixed-size
serialize/unserialize ABI. Each worker loads a private image of the shared
object, so no global lock serializes core execution.

The production adapter benchmark on `ms02` reports a 12,912-byte emulator
state (50,619 bytes in the complete JSON snapshot), a byte-identical
snapshot--restore--snapshot fixpoint, 0.351 microseconds per snapshot
(2.85 million/s), and 0.768 microseconds per restore (1.30 million/s). The
complete 120-frame probe plus restore costs 2.82 ms. These measurements are
in `quicknes-bench.txt` under the final result directory.

The first QuickNES 30K curve used a 32-job deterministic window per worker. It
reached 2,771.959 executions/s at 24 workers but only 75.5% efficiency. This
negative result is preserved in `quicknes-window32-authoritative-curve30k-v2`.
Increasing the bounded reservation window to 64 jobs per worker was the
smallest further coordination change that cleared the gate; executor prefetch
remains eight jobs.

## Authoritative sole-QuickNES 30K curve

| Workers | Wall time | Executions/s | Speedup | Efficiency | Frames | Frames/s |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 176.852 s | 169.633 | 1.000x | 100.0% | 7,291,005 | 41,226.59 |
| 2 | 98.468 s | 304.667 | 1.796x | 89.8% | 8,057,264 | 81,826.18 |
| 4 | 41.650 s | 720.287 | 4.246x | 106.2% | 6,390,241 | 153,426.86 |
| 8 | 22.100 s | 1,357.471 | 8.002x | 100.0% | 6,693,762 | 302,886.16 |
| 16 | 12.089 s | 2,481.658 | 14.630x | 91.4% | 6,390,055 | 528,597.64 |
| 24 | 8.628 s | 3,477.006 | 20.497x | **85.4%** | 6,091,786 | 706,039.20 |

The cleaned 24-worker result is 104.43x the documented 33.294185/s baseline
and 20.88x the required 166.471/s absolute threshold. Both acceptance gates
therefore pass: efficiency is above 80%, and absolute throughput is above 5x
baseline.

The run from the completed 24-worker archive and its snapshot checkpoint
started with 61,481 retained entries and admitted another 30,000 executions in
13.741 seconds: 2,183.256 executions/s and 620,525.82 frames/s. It finished
with 123,036 retained entries. The mature archive remains 65.58x faster than
the original 24-worker baseline; archive depth does not restore the serial
ceiling.

## QuickNES exact replay and live repeat

All six table campaigns replayed from genesis with `replay_verified: true`.
That verdict requires the rebuilt report, archive, and snapshot checkpoint to
match the live files byte-for-byte. The recorded stream hashes are:

| Workers | Stream SHA-256 | Exact replay |
| ---: | --- | --- |
| 1 | `0293eb656d7c26876d5ed333bfa38710ee14f9a5d4f751d019d67a8ba3442027` | pass |
| 2 | `6aad3c2226a2e1e8f3faea4b80bc606fe786714bb38bd99e6f647db409b71a31` | pass |
| 4 | `143416020ea8f9fd377ff8ddbcd6c0b91e8d884b0f1028f93daaa47176db03f2` | pass |
| 8 | `d70156e925680823edafff145e7954079fb74ce3c8a191ad23335812c4e456fc` | pass |
| 16 | `ec29ff5d67db0b0cd54fb64007b1453133b990ab3e99c227405d23b63e4ff37e` | pass |
| 24 | `9a8332eb32540fb76079f7dfca843a63d0b2338037fa56cefe1818ea653eb55b` | pass |

An independent second 24-worker live run produced the same stream, archive,
report, and snapshot bytes (`cmp` on all four files) and itself replayed
exactly. Its throughput was 3,487.007 executions/s. The cleaned binary also
replayed the preserved pre-cleanup 1-worker and 24-worker QuickNES gate
streams, proving that removal of the retired backend did not change the
QuickNES target identity. The mature-archive stream also replayed exactly.

## QuickNES-visible search behavior

Window size changes when a batch observes admitted archive state, so it is
expected to change the deterministic search trajectory. These are two 24-worker,
30K campaigns using the same QuickNES binary, ROM, seed, and search policies:

| Counter | Window 32 | Final window 64 |
| --- | ---: | ---: |
| progress watermark | world 0, level 1, progress 115 | world 0, level 1, progress 48 |
| first 1-1 flag execution | 10,996 | 17,712 |
| first 1-2 execution | 11,781 | 18,151 |
| max 1-1 scroll bucket | 197 | 197 |
| retained | 72,312 | 61,481 |
| deaths | 8,119 | 5,162 |
| duplicate skips | 76 | 41 |
| replacement frames displaced | 12,512 | 14,107 |

No search policy or game knowledge was changed or tuned for these results.
Only the deterministic coordination window changed.

## Deep-archive selector-index follow-up

An exploratory 24-worker run from SMB genesis exposed a second scale-dependent
cost after the archive grew far beyond the 30K acceptance campaigns. The run
was stopped on request after 366,686 stream jobs (366,564 in the last sidecar
sample), 1,675 seconds, and 792,251 retained candidates. It reached 2-2. The
first entry into each level was:

| Level | Elapsed | Executions |
| --- | ---: | ---: |
| 1-1 | 0 s | 1 |
| 1-2 | 5 s | 18,151 |
| 1-3 | 69 s | 85,717 |
| 1-4 | 207 s | 151,199 |
| 2-1 | 286 s | 176,266 |
| 2-2 | 604 s | 245,010 |

That exploratory binary carried a level-transition logger which called the
read-only `live_progress` full-archive scan after every admission. At roughly
800K entries the logger itself became an O(archive-size) serial stage, so its
3--4-core utilization was not production behavior. The partial evidence is
preserved on `ms02` in
`results/experiment-e2e/ms02-genesis-24-1m-1h`; the logger was never added to
the production branch.

The uninstrumented production binary was then profiled from the last complete
checkpoint: 350,000 source executions and 754,944 retained entries. In a
10,000-job sample, 96.4% of parent-selection time was spent in
`walk_to_cell`. Every draw scanned the deepest walk class and reconstructed
the same live room/band/cell hierarchy from 587K active entries and 404K
occupied cells.

The retained change keeps that hierarchy as derived state. Inserts,
displacements, exhaustion, productivity, and deterministic counter resets
update ordered `BTreeMap`/`BTreeSet` indexes; the seeded draw still sees the
same ordered group set at every depth. The serialized archive, public API,
selector policy, random draws, and accounting are unchanged. A unit test
drives exhaustion and reactivation while comparing every cached walk and the
post-walk RNG state against the original full scan.

The same 24-worker, 10,000-job mature campaign before and after measured:

| Metric | Full scan | Derived live index | Change |
| --- | ---: | ---: | ---: |
| search-loop wall time | 10.181 s | 8.516 s | 1.195x faster |
| search-loop executions/s | 982.267 | 1,174.257 | 1.195x |
| parent selection | 4.330 s | 0.208 s | 20.84x faster |
| worker busy fraction | 80.06% | 96.45% | +16.39 pp |
| total 24-core CPU occupancy | 82.66% | 97.75% | +15.09 pp |

The complete CLI rate, which additionally materializes and writes the final
755K-entry report and 12 GiB checkpoint after the search has stopped, improved
from 461.647 to 501.587 executions/s. Those final-output costs are intentionally
excluded from the worker-saturation calculation.

A short fixed 2,000-job mature curve records the hybrid-core shape and the
one-time index-build sensitivity:

| Workers | Search-loop executions/s | Frames/s | Worker busy fraction |
| ---: | ---: | ---: | ---: |
| 1 | 89.604 | 40,927.42 | 99.6% |
| 2 | 137.577 | 80,408.24 | 98.9% |
| 4 | 353.444 | 151,686.69 | 97.3% |
| 8 | 522.576 | 291,655.28 | 93.4% |
| 16 | 675.228 | 454,334.03 | 80.3% |
| 24 | 769.372 | 660,552.51 | 86.4% |

The longer 10K point is the saturation result because it amortizes rebuilding
the derived index from the imported tree: workers occupy 96.45% of their
available core-time and the coordinator lifts total machine CPU occupancy to
97.75%.

The optimized 10K stream, archive JSON, and snapshot checkpoint matched the
full-scan artifacts byte-for-byte. Formal replay also passed with
`replay_verified: true`; its stream SHA-256 is
`31af97d732615bfbdd1b7c6b325100036dc854f8285ad3c2c8e7f761e445707a`.
The replay CLI now borrows the decoded source archive instead of cloning it and
compares multi-gigabyte artifacts through bounded buffers instead of reading
both files wholly into memory. This removed two replay-only memory spikes; it
does not change campaign state or recorded bytes.

## msr1 QuickNES scaling

The same short protocol was also run on the 12-core arm64 `msr1`: genesis,
seed `6672613057367113729`, action limit 512, and 2,000 executions per point.
Its locally built pinned QuickNES core has SHA-256
`5a65587bf6faa5bc86ea05648b81b0e01e5f639ea5020166a14b5d96a92a3db0`.

| Workers | Executions/s | Speedup | Efficiency | Process cores used |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 114.108 | 1.000x | 100.0% | 1.01 |
| 2 | 235.339 | 2.062x | 103.1% | 2.00 |
| 4 | 412.508 | 3.615x | 90.4% | 3.90 |
| 8 | 671.451 | 5.884x | 73.6% | 7.39 |
| 12 | 781.984 | 6.853x | 57.1% | 10.41 |

All five streams replayed exactly. The 12-worker point is 23.49x the historical
33.294 executions/s old-box baseline, but that ratio is cross-host context,
not a controlled same-machine before/after comparison. Raw artifacts are in
`/root/harmony-searcher-scaling/experiment-perf` on `msr1`.

## Validation gates

The root workspace passed `cargo build --all-features`, 1,224 nextest tests,
clippy with warnings denied, formatting, and `cargo deny check`. The standalone
Dissonance workspace passed the same five gates with 73 nextest tests. The
unsafe `machine` crate passed `cargo +nightly miri test -p machine` (6 tests),
including the libretro function-table seam, fixed-buffer state, direct-RAM
access, padding canonicalization, and restore fixpoint.

## Backend retirement and distribution gate

All live TetaNES machine code, dependencies, lockfile entries, selectors,
commands, tests, and backend-specific CI support were removed. Dissonance has
one direct QuickNES NES path and fresh v2 stream, checkpoint, and evaluator
fixture identities. Consonance contained no TetaNES implementation or
integration to delete. Historical sections above intentionally retain the
name of the core they measured; their execution counts and fixtures are not
QuickNES targets.

QuickNES is not vendored or bundled. Upstream carries mixed GPL-2.0-or-later
and LGPL-2.1-or-later source notices plus a top-level GPL version 2 license.
Distribution with Harmony remains blocked pending a licensing review of those
notices and GPL-version compatibility. This experiment's separately built
core does not waive that gate.

A fresh `dissonance-fixture-private-v2` / `dissonance-fixture-challenge-v2`
genesis fixture was generated from the final 24-worker archive and verified
against the pinned core. Verification certified trace SHA-256
`3efe88149bb1565563dd2f85e68be5f53d5bc4d8162a84f1a8e117a1e904ae70`
and checkpoint SHA-256
`7d92c2bf53a8d017ea0715e0a360b46477a1f307979bdca109287a60083e6022`.
No retired-core fixture or cross-core conversion is retained.
