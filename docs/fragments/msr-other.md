> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

# MSR disposition fragment — class `other`

Host-identity / silicon-inventory MSRs that fit none of the named time, power, or perf
classes: the Protected Processor Inventory Number pair. Match rule against
`arch/x86/include/asm/msr-index.h` @ v6.18.35: every name matching `MSR_PPIN*` — exactly
`MSR_PPIN_CTL` (0x4E, msr-index.h:92) and `MSR_PPIN` (0x4F, msr-index.h:93). The AMD twins
`MSR_AMD_PPIN_CTL`/`MSR_AMD_PPIN` (0xC00102F0/F1, msr-index.h:635-636) do not match the
rule and AMD is a task-06 non-goal; they remain covered by the contract §1 default-deny
filter. Reference-set membership is via clause (c) only: neither index appears in any of
KVM's static arrays at v6.18.35 (`msrs_to_save_base`, `msrs_to_save_pmu`,
`emulated_msrs_all`, `msr_based_features_all_except_vmx` — verified by grep of
`arch/x86/kvm/x86.c`) nor in INTEGRATION.md §7's named list. Blanket policy: **deny-gp on
both directions for both entries**, logged loudly per contract §1. Rationale for the
class: PPIN is a fused, per-silicon serial number — the one MSR whose value is by
definition unique to the physical host — so any exposure, even read-only, plants an
unfreezable host-identifying value in the guest-visible surface: the same guest run on two
hosts diverges, and §7's CPUID-stability mandate (one frozen, versioned, hashed model —
never inherit the host's values) is broken; PPIN_CTL additionally reflects host-BIOS
enable/lockout posture (bit 0 = LockOut, bit 1 = Enable_PPIN), which varies across
machines. Deny is architecturally faithful — the frozen CPUID model hides PPIN
(CPUID.(EAX=07H,ECX=1):EBX[0] = 0, the bit Linux's scattered.c:29 reads) and the
boot-baseline fragment freezes MSR_PLATFORM_INFO (0xCE) with bit 23 (PPIN_CAP) = 0, so on
a CPU that enumerates no PPIN these MSRs #GP (gate-5 consistency: no half-exposed
feature). It is also boot-safe and necessary independent of CPUID hiding: Linux
model-matches legacy Xeons with *no* CPUID enumeration (`ppin_cpuids`, common.c:120) and
probes with `rdmsrq_safe`/`wrmsrq_safe` in `ppin_init` (common.c:140), cleanly clearing
the feature on #GP — so the injected #GP, not leaf masking, is the enforcement that holds
for any frozen model choice, and the task-04 guest boots unaffected. The guest can never
establish state in this class, so nothing is captured in `vm_state` (INTEGRATION.md §4).

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_PPIN_CTL | 0x4E | deny-gp | deny-gp | §7 CPUID stability: enable/lockout control for the host silicon serial — readback leaks host-BIOS PPIN posture (varies per machine, unfreezable), and a write could arm PPIN reads; denying both directions keeps PPIN permanently unreachable, and #GP is what Linux's rdmsrq_safe/wrmsrq_safe probe expects on a non-PPIN part, so the task-04 guest boots clean. | msr-index.h:92 @ v6.18.35; absent from arch/x86/kvm/x86.c static MSR arrays @ v6.18.35 (clause-c entry); arch/x86/kernel/cpu/common.c:120,140 (ppin_cpuids, ppin_init; bit 0 LockOut / bit 1 Enable) @ v6.18.35; Intel SDM Vol 4 Table 2-2 (MSR_PPIN_CTL); lwn.net/Articles/880824; intel/ModernFW Ppin.c |
| MSR_PPIN | 0x4F | deny-gp | deny-gp | §7 CPUID stability: the Protected Processor Inventory Number is a unique per-silicon serial — reading it hands the guest the physical host's identity, a value that differs across hosts and therefore can never be part of a frozen, hashed determinism surface; writes are architecturally meaningless (read-only MSR) and likewise #GP. Architecturally consistent with PPIN_CAP = 0 (CPUID.(07H,1):EBX[0] hidden; MSR_PLATFORM_INFO[23] = 0 per the boot-baseline fragment's 0xCE row). | msr-index.h:93 @ v6.18.35; absent from arch/x86/kvm/x86.c static MSR arrays @ v6.18.35 (clause-c entry); arch/x86/kernel/cpu/scattered.c:29 (CPUID.(07H,1):EBX[0] enumeration) @ v6.18.35; Intel SDM Vol 4 Table 2-2 (MSR_PPIN, MSR_PLATFORM_INFO[23] PPIN_CAP); lwn.net/Articles/880824; intel/ModernFW Ppin.c; cross-ref fragment msr-boot-baseline.md (MSR_PLATFORM_INFO 0xCE, bit 23 = 0) |
