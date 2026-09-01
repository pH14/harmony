# vmm-backend implementation

This crate provides architecture-typed VM backends. The portable contract is
small: enter the guest with `Backend::run`, classify the exit, inject modeled
interrupts where the architecture supports them, and save or restore backend
state.

Virtual time is not measured in this crate. Backends expose ordinary VM exits;
`vmm-core` assigns each handled exit a deterministic virtual-time duration.
The Linux x86 backend configures the frozen CPU/MSR surface, userspace exits for
modeled instructions, deterministic entropy, and device isolation. The arm64
KVM and HVF backends expose their architecture-specific exit surfaces behind
the same typed boundary.

The patched-KVM series contains only the CPU-contract patches that remain
necessary for deterministic guest-visible state. It has no timing-counter,
preemption, or stepping extensions.

Portable mock backends exercise the contract on every host. Linux KVM and macOS
HVF live tests provide platform evidence. Any crate path containing `unsafe`
is included in the repository Miri gate through an interpreter-reachable seam.
