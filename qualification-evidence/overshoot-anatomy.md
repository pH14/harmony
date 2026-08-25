# Every failing arm in the campaign is one overshoot, and every one recovered

## What the records said, and what they left out

The campaign harness replays each target: it arms the same target twice and requires
both arms to land exactly and to agree on the landed state. It records the first arm's
work count, skid and stop reason in full, and of the replay arm it records only the
landed digest and whether that digest matched. So its failures came in two apparent
classes - two arms where the first arm overshot, and three where the first arm was
perfect and the replay disagreed - with no way, from the record alone, to say what the
replay had done.

They are one class. All five are overshoots, and in all five the other arm of the same
target landed exactly on it.

## How the replay's work count was recovered

The payload is `mov ecx,300000 ; 1: dec ecx ; jnz 1b ; hlt` and the digest is FNV-1a
over RIP and RCX, so a digest names a stop point. Two are reachable:

| RIP | RCX | meaning |
| --- | --- | --- |
| 0x1006 | 300000 - work | at the `dec`, having just retired the `jnz` that made the work count - a single-step landing |
| 0x1008 | 300000 - work - 1 | at the `jnz`, having just retired the next `dec` - an overflow stop |

The model was fitted before it was used. Seven digests in the campaign records have a
work count the records state outright - five exact landings and the two recorded
overshoots - and the model reproduces all seven. Over the whole reachable range the
dictionary is 600,002 digests for 600,002 work-count and stop-point pairs, with no two
pairs sharing a digest, so an inversion is unique. `box/stage2/digest-model.py` is the
script; it refuses to invert anything if the validation set fails.

Two independent checks agree with it. The digest is a pure function of the work count
landed on: 97,174 distinct work counts were landed on exactly during the campaign and
97,135 of them were landed on more than once, and not one produced two different
digests (`box/stage2/digest-consensus.py`). And for the two overshoots the record does
describe, the inversion returns exactly the `work_at_preempt` the harness wrote down.

## The five

| core | idx | target | period | arm | stopped at | past the target | skid |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 15 | 35919 | 27,325 | 11,133 | first | 38,949 | +11,624 | 27,816 |
| 15 | 28164 | 85,981 | 69,789 | first | 107,384 | +21,403 | 37,595 |
| 5 | 30069 | 66,045 | 49,853 | replay | 100,285 | +34,240 | 50,432 |
| 9 | 15257 | 67,121 | 50,929 | replay | 103,666 | +36,545 | 52,737 |
| 11 | 33511 | 69,772 | 53,580 | replay | 110,305 | +40,533 | 56,725 |

Every one stopped at RIP 0x1008, the overflow's stop point, never at a single-step
landing. The largest guest-mode skid this chip produced is 56,725, which is 3.50 times
the sealed margin of 16,192 - larger than the 37,595 the record showed on its face.

## Detection and recovery

**Detection held five times out of five.** Not one overshoot was recorded as a landing.
The two on the first arm were caught by the harness's own `overshoot` test; the three on
the replay were caught by the digest comparison, which is what the replay is for. All
five arms carry `ok = 0`.

**Recovery held five times out of five, at the sealed margin, in production data.** For
each of the five, the other arm of the same target - same target, same margin, same core,
moments apart - landed exactly on the target and produced the consensus digest for it.
That is the re-arm the handling calls for, and it succeeded every time it was needed.
The measurement was not staged for the purpose; it fell out of the campaign's own replay
discipline.

## What this changes and does not change

The margin stays 16,192. Covering a 56,725 skid would need a margin near 114,000, which
would single-step seven times more work on every landing and still would not be a bound,
since the tail is unbounded by rr's own characterisation of Zen. The chip is usable
because the overshoot is loud and the retry works, and both of those are now measured
rather than argued.

What it does change is the honest statement of the tail: not "37,595 once", but five
events with a maximum of 56,725, and a rate stated against the arms that were exposed
to one.
