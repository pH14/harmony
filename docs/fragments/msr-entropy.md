> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

# MSR disposition fragment: class `entropy`

Class `entropy` covers MSR-borne nondeterministic host-event counters — entropy side
doors outside the RDRAND/RDSEED instructions, which PLAN.md's trap table already routes
to the seeded PRNG stream over the hypercall channel (the port-I/O doorbell, INTEGRATION.md
§1). Its single member is `MSR_SMI_COUNT`
(0x34): a model-specific (Nehalem+, no CPUID enumeration bit) read-only counter of System
Management Interrupts, i.e. asynchronous host firmware events whose arrival is pure
real-world nondeterminism — exactly the kind of free-running host-activity counter
turbostat reads to monitor the host, and an entropy/timing channel if it ever reached the
guest. Match rule: every name matching `MSR_SMI_COUNT` in
`arch/x86/include/asm/msr-index.h` at v6.18.35 (one MSR, msr-index.h:913); it is in the
reference set via `emulated_msrs_all` (`arch/x86/kvm/x86.c:430`, behind
`KVM_GET_MSR_INDEX_LIST`). KVM's own emulation already decouples it from the host — RDMSR
returns `vcpu->arch.smi_count`, the count of *virtual* SMIs KVM injected (x86.c:4502),
and guest WRMSR #GPs (host-initiated writes only, x86.c:4134) — and this VMM never
delivers SMM/SMIs to the guest at all, so the only deterministic readback would be a
constant 0 the guest has no enumerable need for. Under the contract's default-deny
posture (hide/deny unless explicitly justified) the class is denied in both directions;
because the MSR is model-specific with no CPUID feature bit, probing guests already use
`rdmsr_safe`-style access and #GP is the behavior they are built to absorb. Per the
contract's §1 policy, `deny-gp` means: `KVM_X86_SET_MSR_FILTER` +
`KVM_CAP_X86_USER_SPACE_MSR` with `KVM_MSR_EXIT_REASON_FILTER` exit to userspace, log MSR
index and guest RIP, then inject #GP — never a silent in-kernel fault.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_SMI_COUNT | 0x34 | deny-gp | deny-gp | Closes §7's power/frequency host-event-counter vector (generalized to SMIs): the count of host firmware SMIs is asynchronous real-world nondeterminism usable as an entropy/timing channel; no SMM is ever delivered to the guest so the deterministic value is a constant with no enumerable consumer — default-deny; write-#GP also matches silicon (read-only counter) and KVM (guest writes rejected, x86.c:4134) | linux-6.18.35 arch/x86/kvm/x86.c:430 (emulated_msrs_all), 4502 (RDMSR returns vcpu->arch.smi_count), 4134 (guest WRMSR #GP unless host-initiated); arch/x86/include/asm/msr-index.h:913; Intel SDM Vol. 4 Table 2-2 (MSR_SMI_COUNT, 34H, Nehalem+); tools/power/x86/turbostat/turbostat.c:1789 (host SMI counting); INTEGRATION.md §7 (default-deny, power/frequency); PLAN.md trap table (entropy → seeded stream) |
