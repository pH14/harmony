# Architecture boundary

Consonance shares one deterministic run-loop policy across x86-64 and arm64
while keeping architecture-specific registers, exits, completions, and device
contracts typed.

## Shared engine

`vmm-core::Vmm` owns guest memory, one virtual clock, timers, entropy,
snapshots, the control protocol, and the total ordering of serviced exits.
`vmm-backend::Backend` enters a guest with `run`, saves and restores typed
vCPU state, maps memory, injects modeled interrupts, and reports exit counts.
Neither interface measures time.

Every serviced exit is normalized by the selected `Vendor` implementation and
advances virtual time by its frozen integer duration. Common exits are matched
exhaustively in the engine; architecture exits are matched exhaustively in the
vendor module.

## x86-64

The x86 backend exposes CPUID, MSR, port-I/O, deterministic-instruction, and
xAPIC surfaces. Linux KVM composition installs the frozen CPU/MSR contract
before entry. RDTSC/RDTSCP values come from the virtual clock; RDRAND/RDSEED
values come from the seeded stream.

## arm64

The arm64 backend exposes sysreg, MMIO, WFI, and GIC/timer surfaces. Linux KVM
and macOS HVF use the same board and normalized-exit contract. The stock arm64
KVM skeleton intentionally has no interrupt-delivery fabric; this is the settled
project design.

## Portability rule

Portable code cannot depend on an architecture binding hidden behind only an OS
cfg. Linux x86 substrate uses
`cfg(all(target_os = "linux", target_arch = "x86_64"))`; Linux arm64 uses
the corresponding `aarch64` cfg. Cross-architecture clippy jobs enforce this
boundary.
