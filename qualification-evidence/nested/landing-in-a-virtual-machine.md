# Landing inside a virtual machine

The landing procedure works inside a virtual machine on this chip, and lands on the same
state as bare metal. Across 6,960 arms measured both ways, every arm that landed exactly
landed on a bit-identical state. Skid does not grow under nesting: the distributions agree
to within a few counts through the 99.9th percentile. The margin this chip's baseline seals
holds, with the deep tail unmeasured.

This rests on patch `0009`, without which the count the whole procedure reads is wrong.
See `nested-on-this-chip.md`.

## The arrangement

The virtual machine gets one virtual CPU pinned to the host's isolated core, `-cpu
host,pmu=on`, and runs the patched kernel and KVM modules from a copy of the host's own
root filesystem. It has no isolation of its own — a single CPU carrying its whole kernel
and every task on it — so the guest is the unfavourable case, not a tuned one.

The instrument is `ae3-forceexit`, the same binary on both sides, driven with the same
seeds and the same arm counts so the two runs draw the same targets in the same order. It
arms a guest-filtered counter to overflow at `target - margin`, takes the in-kernel
preemption exit when it fires, single-steps until the count reaches the target exactly, and
records the landed register state as a digest. An overflow that carries past the target is
recorded as an overshoot and fails the arm; the margin is never quietly enlarged.

Two runs on each side, an hour apart on the same box:

- **A** — 5,000 arms at margin 16,192, each target landed twice and the two landed states
  compared. This is the sealed margin.
- **B** — 2,000 arms at margin 3,072, a margin deliberately too small, to push arms into
  overshoot and show the distribution's reach cheaply.

## What was measured

| | bare metal | inside a virtual machine |
|---|---|---|
| A: arms | 5,000 | 5,000 |
| A: landed exactly | 5,000 | 5,000 |
| A: overshot | 0 | 0 |
| A: repeated landings with identical state | 5,000 of 5,000 | 5,000 of 5,000 |
| A: harness verdict | pass | pass |
| B: overshot at a margin of 3,072 | 26 of 2,000 | 14 of 2,000 |

Skid, over the arms whose target sat above the margin so the overflow path ran:

| | bare metal | inside a virtual machine |
|---|---|---|
| A: arms counted | 4,159 | 4,159 |
| A: minimum | 1,588 | 1,787 |
| A: median | 2,907 | 2,906 |
| A: 99th percentile | 3,605 | 3,404 |
| A: 99.9th percentile | 5,429 | 5,633 |
| A: maximum | 6,878 | 7,376 |
| A: standard deviation | 172 | 168 |
| B: median | 2,907 | 2,909 |
| B: maximum | 6,775 | 6,704 |

## Landing on the same state

The two sides draw the same targets, so the landed state can be compared arm by arm.

- **Run A: 5,000 arms compared, zero digests differ.** Every target landed on the same
  register state on metal and inside the virtual machine.
- **Run B: 40 of 2,000 differ, and all 40 are arms that overshot** — 26 on metal, 14 in the
  virtual machine, with no arm in both sets. An overshoot stops past the target, so its
  state is a different state by construction. Among the 1,960 arms where both sides landed
  exactly, every digest agrees.

Records under `landing/`; `compare.py` and `skid.py` produce these figures from them.

## The margin

The sealed margin of 16,192 holds. The largest skid the virtual machine produced over 4,159
overflow arms was 7,376, leaving a factor of 2.2. The two distributions are the same
distribution to the resolution these runs have.

That resolution stops well short of the tail. The metal campaign that set this margin ran a
million landings and reached 37,595 on an isolated core; 5,000 arms cannot see that far.
Reaching it inside a virtual machine at the rate below would take about thirty hours. What
these runs establish is that the bulk of the distribution does not move under nesting and
that nothing new appears within their reach, not that the tail is the same shape.

## What it costs

Run A took 897 seconds on metal and 3,673 seconds inside the virtual machine, both for
10,000 landings: 90 milliseconds against 367, a factor of 4.1. The cost is single-stepping,
which takes one transition into and out of the guest per instruction, and each of those
transitions is more expensive under nesting — partly the nesting itself, partly the 3.6
microseconds patch `0009` adds to each one.

The records carry the distance stepped on every arm, as the target less the work the
overflow stopped at, so the per-step cost is a division rather than an estimate:

| | bare metal | inside a virtual machine |
|---|---|---|
| A: steps per landing, median | 13,280 | 13,281 |
| A: steps per landing, mean | 12,402 | 12,405 |
| A: milliseconds per landing | 89.70 | 367.30 |
| A: microseconds per step | 7.2 | 29.6 |
| B: steps per landing, median | 167 | 164 |
| B: milliseconds per landing | 2.80 | 6.40 |

Run B is at a fifth of run A's margin and a sixtieth of its stepping, because the median
skid of about 2,907 comes off both margins and leaves far less of the smaller one to step.
Fitting the two runs against each other, with run A's per-arm cost spread over its two
landings and run B's over one, separates a landing on metal into about 7.2 microseconds per
step and about 1.3 milliseconds of per-arm cost. That 1.3 milliseconds is the harness
setting an arm up, not the free run to the overflow: run B free-runs more work than run A
does, 48,896 units against 36,910, and still costs a thirty-second of it. So the stepping
alone is 89 milliseconds of run A's 90 and 1.5 of run B's 2.8.

Two things follow. A margin of 3,072 lands in 2.8 milliseconds rather than 90, at the price
of a re-arm on about one arm in a hundred, which `skid.overshoot` in the pack records
recovering every time it was tried and which no code does automatically. And an arm whose
target sits below the margin never arms an overflow at all and is stepped from zero: 841 of
run A's 5,000, against 68 of run B's 2,000.

## A compatibility break found on the way

The landing harness would not arm at all at first, failing with `EINVAL` on a host where
everything else worked. Patch `0007` turned the deterministic-intercepts opt-in from a
single enable bit into a mask of instruction classes. The harness passed `args[0] = 1`
meaning "on", which under `0007` names the time-stamp class and nothing else, so the
preemption exit the landing procedure needs was never enabled. The enable succeeded and the
arm was refused.

The harness now reads the supported set with `KVM_CHECK_EXTENSION`, asks for that, and
stops if the preemption class is absent. Any other caller written against the single-bit
form has the same problem, and it is quiet on the enable rather than loud.

## Not settled

- The tail. See the margin section: the runs bound it only to their own reach.
- The guest here runs one virtual CPU. Nothing establishes what several contending virtual
  CPUs on one host core would do.

The suite has since been run end to end inside a virtual machine, reaching the same
verdict as metal — see `the-suite-in-a-virtual-machine.md`.
