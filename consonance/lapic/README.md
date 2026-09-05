# lapic

`lapic` is a pure-logic userspace xAPIC model for the deterministic VMM. It
implements the register file, interrupt priority/delivery, and LVT timer; the
VMM routes APIC MMIO exits here and owns KVM interrupt injection. There is no
in-kernel interrupt controller in this model, and the crate addresses offsets
within the APIC page rather than relocating `IA32_APIC_BASE`.

## Register model

The model exposes a 4 KiB xAPIC MMIO page at the architectural base
`0xFEE0_0000`, with 16-byte-aligned register offsets. It reports the six modeled
LVT entries (timer, thermal, performance monitor, LINT0, LINT1, and error).
CMCI, x2APIC MSRs, IOAPIC, and inter-processor delivery are outside the model.

Reads of unimplemented or write-only registers return zero. Writes to
read-only or reserved-but-in-range registers are ignored; only a misaligned or
out-of-range offset returns `LapicError::BadOffset`. Writable registers mask
reserved bits before storing them. In the single-vCPU model, a self-targeted
fixed-mode ICR write raises a local vector; other destinations have no effect.

## Timer and delivery

All time-dependent methods receive the caller's V-time in nanoseconds. A
non-zero `LapicConfig::timer_hz` fixes the timer input frequency. Initial-count
writes anchor the count at the supplied V-time; the deadline is derived with
integer arithmetic and a ceiling, using the divide configuration. Current count
is derived from elapsed V-time, periodic timers catch up without drift, and
one-shot timers consume their pending count. Unsupported timer modes do not
advertise a deadline.

Interrupt priority is based on vector class (`vector >> 4`) and the TPR/PPR and
ISR state. `has_deliverable` checks software enable and priority; `take_interrupt`
moves the highest eligible vector from IRR to ISR; `eoi` clears the highest ISR
vector. Reserved vectors are rejected by `raise`.

## State and integration

`LapicState` is the versioned plain-data snapshot consumed by `vm-state`. It
contains the register arrays and timer anchors needed to restore identical
readbacks and deadlines; derived deadlines are not stored. Restore validates
the state version, timer frequency, register masks, and timer invariants.

Timer arithmetic is integer-only with wide intermediates and saturation where
appropriate. The crate reads no clock, performs no I/O, and has no backend
dependency. Register, timer, delivery, reset, snapshot, property, and formal
bound tests cover the model.
