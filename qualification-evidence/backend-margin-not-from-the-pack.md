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

Reported, not fixed. Wiring the pack into the backend is a design change and this
program's terms allow code changes only to the pack itself and to defects the box
exposes in the suite. The gap is named here so it is not mistaken for something this
program's numbers already cover.

The consequence for the verdict is narrow: it is a software gap, not a property of the
chip. Every landing number in this program was measured with the margin the pack seals,
16,192, supplied to the measurement harness directly.
