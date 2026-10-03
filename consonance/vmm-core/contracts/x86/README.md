# x86 guest CPU policy

`guest.toml` defines the ordinary x86 machine presented to a Harmony guest on all
physical hosts. It fixes CPUID values, MSR access rules, modeled devices, and
virtual-time durations. The `GenuineIntel` vendor string and family/model fields
are guest-visible compatibility values, not a requirement to run on Intel or a
particular processor. There are no per-vendor or per-microarchitecture files,
CPU identity probes, or microcode pins.

`nested-host.toml` is a separate named overlay for a one-vCPU Intel VMX host
guest. It adds CPUID.1:ECX.VMX, locks IA32_FEATURE_CONTROL with VMX outside SMX
enabled, and exposes the VMX capability MSRs needed by the pinned Linux
`kvm_intel`. Their values come from KVM after installing the nested CPUID model;
the overlay's canonical form, ordinary policy hash, and every capability value
enter its contract hash. A host reporting different VMX capabilities has a
different nested-host contract. Ordinary and nested-host snapshots are rejected
by each other's restore paths before mutation. The ordinary TOML and hash are
unchanged. This profile is experimental until live nested restore qualification
passes; enabling VMX alone does not make live VMX state restorable.

`vmm-core` embeds this file and derives the CPUID model, MSR filter, access
handlers, timing rules, and snapshot contract hash from it. Backend capability
requirements still apply: changing a host cannot supply a missing hypervisor
feature or emulate an unsupported native instruction. The cooperative guest
uses the advertised instruction set. Hidden, untrappable instructions are
outside that surface; their physical absence is not assumed. See
[Determinism](../../../../docs/DETERMINISM.md).

The single CPUID policy hides hardware RNG feature bits. The controlled Linux
guest uses deterministic entropy and paravirtual time; native RNG and timestamp
instructions are outside the cooperative surface.

## Changes and compatibility

Policy version 6 removes the host identity and physical-absence assertions from
the canonical form. It retains guest microcode/CR4 invariants and versions the
cooperative instruction boundary. The contract hash changes, so snapshots from
older policy versions are rejected before restore mutates the VM. Hardware RNG bits now live in the shared table instead of a runtime override.
Production CPUID values, MSR dispositions, and virtual-time durations are unchanged.

Changes to guest semantics require a version bump. `contract_hash` is computed
once per process from the canonical form of the embedded policy. Concurrent first
callers share initialization, and subsequent captures, hashes, and restores copy
the same 32-byte fingerprint without serializing the policy again. A snapshot
saved under a different contract is rejected before restore changes the VM. The old Coffee Lake captures and unused AMD draft remain in Git
history, not in runtime policy or the test matrix.

The separate `nested-host.toml` policy selects VMX or SVM from host KVM support.
VMX adds its capability MSRs and locked feature-control register. SVM uses an
AuthenticAMD identity, a fixed family-19h signature, SVM revision 1 and the
KVM-reported ASID count. Its feature mask exposes NPT, NRIPS, VMCB clean bits,
flush-by-ASID and decode assists, excluding host-time scaling and unrelated
extensions. VM_CR and VM_HSAVE_PA remain native stateful MSRs. AMD HWCR reads
return only the fixed P0-frequency bit required by the Linux invariant-TSC boot
check; writes remain rejected.
SYSCFG reads return zero with DRAM-range modification and memory encryption
disabled; writes remain rejected. The selected
vendor and every exposed capability contribute to the nested contract hash;
the ordinary guest contract remains unchanged.
