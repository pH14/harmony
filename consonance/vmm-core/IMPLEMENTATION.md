# vmm-core implementation

`vmm-core` owns the deterministic run loop and composes the architecture
backend, virtual clock, timers, entropy, devices, snapshots, and control
protocol.

## Virtual time

Every successful backend exit is classified and assigned an integer virtual
nanosecond duration by `VirtualTimeConfig`. The run loop advances exactly one
`VClock` with that duration before dispatching the exit. Scheduled host events
whose deadline is now due are applied at that exit boundary. No host clock,
frequency, performance counter, or instruction count contributes to virtual
time.

When a halted guest has a modeled timer pending, the run loop advances the same
clock directly to the next deadline and delivers the event. A future host event
cannot create an execution boundary by itself; if the guest terminates first,
the schedule is reported unsatisfiable.

## Deterministic state

All state-affecting collections have deterministic order. Entropy is derived
from the caller-provided seed. Snapshots serialize the accumulated virtual
nanoseconds, guest-clock parameters, backend state, devices, timers, entropy,
and control state. The wire version is bumped whenever that format changes.
A snapshot is available after every completed exit except while a multi-exit
instruction completion is staged.

## Architecture composition

x86 and arm64 share the run-loop policy while retaining typed register and exit
models. Linux KVM and macOS HVF constructors install the appropriate frozen
contract before guest entry. The arm64 KVM skeleton intentionally has no
interrupt-delivery fabric, per the settled project ruling.

Portable unit/property tests cover exit advancement, event ordering, snapshot
round trips, protocol behavior, and both architecture compositions. Live gates
cover Linux x86 KVM, Linux arm64 KVM, and macOS arm64 HVF.
