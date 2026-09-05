<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

# vtime

`vtime` provides the deterministic virtual clock and timer primitives used by
the VMM. It is pure logic: callers supply all time inputs, and the crate has no
host-clock or hypervisor dependency.

`VClock` stores virtual nanoseconds and advances only by explicit integer
deltas. Guest counter ticks are derived from the configured frequency and base
with integer arithmetic. Addition saturates at `u64::MAX`.

`TimerQueue` stores one-shot and periodic deadlines in a `BTreeMap`, ordered by
deadline and insertion sequence. Equal deadlines fire FIFO. Rescheduling a
token replaces its old entry; periodic timers re-arm from the fired deadline,
so late delivery does not accumulate drift. A zero period is rejected and a
deadline beyond the representable range drops the periodic timer.

`IdlePlanner` returns the deterministic gap from the current clock to the next
deadline. `pvclock` defines the 4 KiB guest page, its seqlock stamping protocol,
and canonical reads for the guest-visible clock projection.

The crate is used by `vmm-core`, which assigns per-exit deltas and joins timer
delivery to the backend event loop.
