# What the nonzero shard exit codes meant, and what re-arming recovers

## The exit codes: the summary line was incomplete, the exit codes were right

Five of the campaign's eight shards exited 1. Only one of them, core 15, showed anything
in its summary line: `overshoot=2`. The other four - cores 3, 5, 9 and 11 - printed
`arms=62500 preempt=62500 exact=62500 overshoot=0` and still exited 1.

The exit code is `all_ok ? 0 : 1` and `all_ok` is cleared by any arm that fails, where an
arm passes only if

    landed_exact && !overshoot && (period == 0 || preempt_exit) && replay_match

The summary line carries no replay quantity at all: `n_overshoot` counts the first arm's
overshoot flag and nothing else. So an arm that failed only on `replay_match` is invisible
in the summary and decisive for the exit code. Recomputed from the per-arm records, the
failing arms are exactly:

| shard | rc | failing arms | the failing term |
| --- | --- | --- | --- |
| core1 | 0 | 0 | |
| core13 | 0 | 0 | |
| core7 | 0 | 0 | |
| core3 | 1 | idx 61737, target 71547 | `replay_match=0`; first arm exact |
| core5 | 1 | idx 30069, target 66045 | `replay_match=0`; first arm exact |
| core9 | 1 | idx 15257, target 67121 | `replay_match=0`; first arm exact |
| core11 | 1 | idx 33511, target 69772 | `replay_match=0`; first arm exact |
| core15 | 1 | idx 28164 and idx 35919 | `overshoot=1` on the first arm, both |

Every shard's exit code is accounted for and no shard exited 0 with a failing arm. This
is not benign in the sense of nothing having happened: each of those four is a real
failing arm. It is benign in the sense that the exit code told the truth and the summary
line did not, which is why every number in this program is recomputed from the per-arm
records.

## What the four replay failures were

The record does not describe the replay arm's landing, only its digest. The digest names
a work count for this payload, so it was inverted (`overshoot-anatomy.md`). All four are
overshoots of the replay arm:

| shard | target | replay stopped at | past the target | skid |
| --- | --- | --- | --- | --- |
| core3 | 71,547 | 85,239 | +13,692 | 29,884 |
| core5 | 66,045 | 100,285 | +34,240 | 50,432 |
| core9 | 67,121 | 103,666 | +36,545 | 52,737 |
| core11 | 69,772 | 110,305 | +40,533 | 56,725 |

So the campaign has six failing arms and they are one phenomenon: a late overflow. Two
were on the first arm and four on the replay.

## The inversion is scored, not trusted

`ae3-instr` records the replay arm's own landed work count as well as its digest, so on
its records the inversion can be marked. Over the 7,000 replayed arms of the two
overshoot-demonstration runs it agrees with the record 7,000 times, disagrees 0 times,
and finds every digest in its dictionary. `box/stage2/digest-validate.py`.

## Core 15 is not special

Counting only first-arm overshoots put both on core 15 and made it look like a cluster.
Counting all six spreads them over cores 3, 5, 9, 11 and 15, with two on core 15 and one
on each of the others. The box has one NUMA node and four L3 domains, pairing the
isolated cores as {1,3} {5,7} {9,11} {13,15}; the six events fall in all four domains.
Core 15 shares its L3 with core 13, which had no failures. Device interrupts are
affinitised to the even cores: over the campaign the odd cores took one device interrupt
between them, on core 5. Nothing distinguishes core 15.

## Recovery, measured

**Part A, overshoot made common.** Arming at a margin of 3,072, just above the median
skid of 2,934, makes a late overflow ordinary instead of rare. 5,000 targets, 10,000
arms, every one through the deterministic exit. 54 first arms and 59 replay arms
overshot, so 113 targets needed a re-arm. **All 113 landed exactly.** 111 needed one
re-arm and 2 needed two; none needed three. `box/stage2/overshoot-recovery.json`.

**Part B, the campaign's own overshot target.** Target 85,981, the one core 15 overshot
by 21,403, re-armed at the sealed margin 16,192: 2,000 targets, 4,000 arms, 4,000 exact
landings, zero overshoots, exit 0. The overshoot was transient, not a property of that
target. `box/stage2/overshoot-target-85981.json`.

**And in the campaign itself.** For all six failing arms the other arm of the same
target - same target, same margin, same core, moments apart - landed exactly on the
target. Six for six at the sealed margin, in production data that was not staged for the
purpose.

An overshoot is therefore loud and it is recoverable. It raises `SkidExceeded` in the
planner and a failed arm in the harness, and it is never accepted as a landing; and the
correct response, re-arming the same target, succeeded 113 times out of 113 where
overshoot was common and 6 times out of 6 where it was rare.

## System-management interrupts are not the cause, and cannot be probed here

Every one of the 10,000 arms in part A carries an SMI-received delta of zero, including
all 113 overshooting ones. That reading is weak on its own: `MSR_SMI_COUNT` (0x34) is an
Intel MSR and does not exist on this part, and the AMD PPR's SMI-received event, raw
`0x51002b`, counts nothing here even when the work clock on the same command counts
normally. Recorded as a null result from a probe that cannot be shown to work, not as
evidence of absence.
