# The backend's arm-early margin is a constant, and on this chip it is far too small

## The finding

`consonance/vmm-backend/src/run_until.rs` arms every overflow at `deadline - SKID_MARGIN`
with

    pub(crate) const SKID_MARGIN: u64 = 256;

Nothing reads the measured margin out of a chip pack: `docs/chips/det-zen3-v1.toml` is
read only by the `cpu-qualification` crate. So the per-chip constant this program
measured and sealed does not reach the code that would use it.

On this chip 256 is not a small error. The smallest guest-mode skid seen in any run of
this program is about 1,349 and the median is about 2,912, so an overflow armed 256
work units early would stop past the deadline on essentially every landing. The
backend's response to that is `SkidExceeded`, a loud internal error, so the failure mode
is a refusal on nearly every deadline rather than a wrong answer. That is the right way
round, and it is still a stop.

## Why the constant is what it is

The code states its own basis:

> the skid is the bounded hardware-PMI latency (~128 retired branches), well inside
> `SKID_MARGIN = 256`

That figure came from a different part. The in-kernel force exit is the same mechanism
here - the patched `KVM_EXIT_PREEMPT` path, which this program built, booted and used
for every landing - and on this silicon the same mechanism produces a median skid of
2,912 and a maximum of 56,725. The mechanism transfers; the number does not. A per-chip
measured-constants pack is the correct answer to that, and the measurement now exists.

## Scope

Reported, not fixed, at the time: wiring the pack into the backend is a design change
and that program's terms allowed code changes only to the pack itself and to defects
the box exposed in the suite. The gap is named here so it is not mistaken for something
that program's numbers already cover.

The consequence for the verdict is narrow: it is a software gap, not a property of the
chip. Every landing number in that program was measured with the margin the pack seals,
16,192, supplied to the measurement harness directly.

## Fixed

The constant is now `DEFAULT_SKID_MARGIN`, documented as the Coffee Lake baseline's
number, and `KvmBackend::set_skid_margin` sets the chip's. The live contract exam reads
the running chip's sealed margin from its pack, which is what made the gap visible as a
failure rather than as prose: on this chip the exam raises `SkidExceeded` on its first
landing at the default. See `nested/README.md`.

What the fix does not decide is which margin to use. 16,192 is twice the maximum skid
of the host-user sampling scope, and landings happen in the guest scope, where skid is
tightly concentrated — median 2,907, standard deviation 166 over the 6,091 arms pooled
from `nested/landing/metal/`. Stepping is `margin - skid`, so headroom is what costs,
and the sealed margin sits about eighty standard deviations above the median:

| margin | arms that overshoot | milliseconds of stepping |
|---:|---:|---:|
| 3,072 | 1.7% | 1.2 |
| 3,500 | 1.0% | 4.3 |
| 4,000 | 0.4% | 7.8 |
| 5,000 | 0.15% | 15.0 |
| 16,192 | none in that sample | 89 |

That sample is 6,091 arms and cannot resolve a rate rarer than about one in six
thousand; the larger campaigns put guest-scope p99.9 at 5,371 to 5,674 and the maximum
at 37,616 on isolated cores.

Choosing a smaller margin needs the re-arm to be automatic, and it is not: `run_until`
raises `SkidExceeded` and stops there. The re-arm cannot be local either — an overshot
guest has already run past the target, so recovery means restoring and re-running, which
belongs above this trait. Note that re-arming is already required at the sealed margin:
`skid.overshoot` in the pack records 62 of 1,558,014 arms overshooting at 16,192. The
larger margin buys a lower rate, not a bound.
