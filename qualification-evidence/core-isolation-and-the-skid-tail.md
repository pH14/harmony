# Core isolation is what bounds the guest-mode skid tail

The campaign left one thing unexplained: the guest-mode skid has a bulk around 2,900 and
a tail that runs to 56,725, and nothing in the records said what put an arm in the tail.
The supplement answers it. The tail is the kernel's own work on the measurement core.

Records: `box/stage2/supplement/analysis.txt`, recomputed from the per-arm records by
`box/stage2/campaign-analysis.py`; shard records `box/stage2/supplement/core*.json.gz`.

## The run

Fifteen shards, one per core, cores 0,1,2 and 4 through 15. Core 3 was left free and
carried the patched-kernel enforcement work. Each shard armed 24,000 targets drawn
uniformly from 16,193 to 100,000 and armed each one twice, so every arm was above the
margin and every arm went through the overflow-then-single-step path: 360,000 targets,
720,000 landings, 720,000 arms exposed to an overshoot.

Seven of the shards ran on cores the kernel keeps off itself: cores 1,5,7,9,11,13,15 are
in `isolcpus`, `nohz_full` and `rcu_nocbs`. Eight ran on ordinary cores the kernel
schedules on: 0,2,4,6,8,10,12,14. Same chip, same run, same posture, same target draw.

## The result

| | isolated cores | ordinary cores |
| --- | --- | --- |
| arms exposed to an overshoot | 336,000 | 384,000 |
| overshoots | 1 | 55 |
| rate | 1 in 336,000 | 1 in 6,981 |
| skid p50 | 2,908 | 2,910 |
| skid p99 | 3,024 | 3,014 |
| skid p99.9 | 5,614 | 5,746 |
| skid p99.99 | 7,539 | 35,346 |
| skid max | 37,616 | 86,738 |

The bulk of the two distributions is the same to within a few counts. The tail is not:
the ordinary cores are 48 times likelier to carry the guest past its deadline, and their
p99.99 is 4.7 times the isolated one. The largest skid this chip produced anywhere,
86,738, is on an ordinary core.

The campaign's own rate, 6 in 838,014 on isolated cores, is consistent with the
supplement's isolated rate rather than with its ordinary one.

## What was ruled out

- SMI. Zero non-zero deltas across all 360,000 arms, from a probe that cannot be shown to
  work on this part; see `shard-exit-codes-and-recovery.md`.
- Guest speed. Branches per TSC tick during the run to the overflow are p1 0.157, p50
  0.795, p99 0.970, and the failing arms spread across 0.49 to 0.98. A slow guest is not
  what puts an arm in the tail.
- Target size. Failing targets run from 30,172 to 99,965 against a draw range of 16,193 to
  100,000, and the mean period of a failing arm, 49,649, is close to the run's
  period-weighted mean of 55,870.
- Silicon. Both populations are on the same socket, in the same run, minutes apart.

## What follows

The pack now carries `core-isolated` as a host condition and stage 0 reads it from
`/proc/cmdline`, checking the core the measurement thread is actually pinned to. Before
this, `core-pinning` said the thread was pinned to cpu3 and nothing said whether the
kernel stayed off cpu3, so a host could satisfy every sealed condition and still measure
in the population with the 48-times-worse tail.

Isolation reduces the tail; it does not remove it. One isolated arm in 336,000 still
overshot, and the isolated maximum is 37,616, well past the 16,192 margin. Detection and
retry remain the contract: 56 of 56 re-arms landed exactly here, and every overshoot in
every run was refused rather than accepted as a landing.

## Two other numbers from the same records

- The digest inversion, which recovers what a replay arm did on records that state only
  its digest, scores 360,000 agreements and 0 disagreements here, on top of the 7,000 it
  already had.
- Repetition: 82,709 distinct work counts were landed on, every one more than once,
  76,987 of them from more than one core, one of them 34 times, 719,915 landings at a
  repeated work count, and not one work count produced two different landed states.
