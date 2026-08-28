# vtime implementation

The virtual-time crate owns a deterministic, saturating nanosecond clock and a
stable timer queue. Time never comes from the host.

## Clock

`VClock` stores the current virtual nanosecond value directly. The VMM calls
`advance(delta_vns)` once for each handled VM exit using the duration selected
by the virtual-time policy. Addition saturates at `u64::MAX`. Guest clock
ticks are derived from the accumulated virtual nanoseconds with integer
arithmetic.

An idle guest advances through the same accumulator. `IdlePlanner` returns the
gap to the next deterministic timer deadline; an overdue deadline produces a
zero-length jump.

## Timers and pvclock

`TimerQueue` orders equal-deadline entries by token, so scheduling and delivery
are stable. Periodic timers advance from their previous deadline and therefore
do not drift.

The pvclock page is a deterministic projection of the clock. Its sequence
protocol prevents torn reads, and canonical stamping clears reserved bytes.

## Verification

Property tests use at least 256 cases for clock monotonicity, saturation, timer
ordering, and encode/read behavior. Kani harnesses prove the clock and idle
arithmetic properties. The crate contains no `unsafe`.
