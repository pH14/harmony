> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

# MSR disposition fragment — class `boot-baseline`

The `boot-baseline` class covers feature-identification and capability MSRs that the
task-04 pinned guest kernel (or KVM's own MSR-list machinery) treats as part of the
machine's frozen identity: the feature-control lock, platform/microcode identification,
topology counts, platform frequency info, and the nested-VMX capability range that KVM
enumerates via `KVM_GET_MSR_FEATURE_INDEX_LIST` (`kvm_init_msr_lists` probes
`KVM_FIRST_EMULATED_VMX_MSR..KVM_LAST_EMULATED_VMX_MSR`, x86.c:7764/7791, x86.h:94–95).
None of these MSRs may ever reflect host values: every readable row is `allow-fixed` with
a constant baked into the versioned baseline (hashed into the determinism gate per the
contract's §6), and every row in this class is architecturally read-only, so all write
dispositions are `deny-gp` (trapped via the MSR filter + `KVM_MSR_EXIT_REASON_FILTER`,
logged with index and RIP, then #GP injected — never silent). The VMX capability range
0x480–0x491 is denied outright in both directions: the frozen CPUID model hides VMX
(CPUID.1:ECX[5]=0; nested virtualization is out of scope), and on a VMX-less CPU reads of
these MSRs #GP architecturally — exposing them would both leak host VMX capabilities and
violate the CPUID↔MSR consistency gate. Column grammar: dispositions are drawn verbatim
from task 06 §3 (`allow-fixed(value)`, `allow-stateful`, `emulate-vtime`,
`emulate-timerqueue`, `emulate-apic`, `deny-gp`, `deny-ignore-write`); Rationale names the
INTEGRATION.md §7 leak vector closed (or `architectural`); kernel citations are
`file:line` at the pinned tag v6.18.35.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| IA32_PLATFORM_ID | 0x17 | allow-fixed(0x0) | deny-gp | CPUID stability: host platform-id/microcode-flags leak; frozen (platform bits 52:50 = 0) in the versioned baseline; microcode loading is disabled in the pinned guest config | Intel SDM Vol.4 Table 2-2 (IA32_PLATFORM_ID); msr-index.h:910 (v6.18.35) |
| MSR_CORE_THREAD_COUNT | 0x35 | allow-fixed(0x0001_0001) | deny-gp | CPUID stability: host core/thread topology leak; pinned to 1 core / 1 thread per docs/PLAN.md ("one vCPU, period"); not in msr-index.h at v6.18.35 — included via the SDM model-specific table | Intel SDM Vol.4 model-specific MSR table (MSR_CORE_THREAD_COUNT); docs/PLAN.md sources-of-nondeterminism table |
| MSR_IA32_FEAT_CTL | 0x3a | allow-fixed(0x1) | deny-gp | CPUID stability: frozen feature surface — lock bit (bit 0) set, VMX-in/outside-SMX and SGX enable bits clear; with the lock set, write-#GP is the architectural behavior, so deny-gp is faithful | x86.c:338 (msrs_to_save_base); msr-index.h:916; Intel SDM Vol.4 Table 2-2 (IA32_FEATURE_CONTROL) |
| MSR_PLATFORM_INFO | 0xce | allow-fixed(bits 15:8 = frozen max non-turbo ratio = frozen-TSC-Hz / 100 MHz; all other bits 0) | deny-gp | Power/frequency: hides host base/turbo ratios; ratio field is pinned to the same frozen TSC frequency as CPUID 0x15/0x16; bit 31 = 0 (no CPUID-fault support advertised, keeping MISC_FEATURES_ENABLES unimplied); turbo/TDP fields zeroed | x86.c:431 (emulated_msrs_all), x86.c:475 (msr_based_features_all_except_vmx); msr-index.h:98; Intel SDM Vol.4 Table 2-2 (MSR_PLATFORM_INFO) |
| MSR_IA32_VMX_BASIC | 0x480 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model (CPUID.1:ECX[5]=0), so #GP is architectural; host VMX capability values never reach the guest | x86.c:445 (emulated_msrs_all), x86.c:7791 (kvm_init_msr_lists VMX probe); x86.h:94; msr-index.h:1216; Intel SDM Vol.3D Appendix A.1 |
| MSR_IA32_VMX_PINBASED_CTLS | 0x481 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU; in reference set only via the kvm_init_msr_lists VMX probe range | x86.c:7791 (kvm_init_msr_lists VMX probe range); x86.h:94–95; msr-index.h:1217; Intel SDM Vol.3D Appendix A.3.1 |
| MSR_IA32_VMX_PROCBASED_CTLS | 0x482 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU; in reference set only via the kvm_init_msr_lists VMX probe range | x86.c:7791 (kvm_init_msr_lists VMX probe range); x86.h:94–95; msr-index.h (0x482); Intel SDM Vol.3D Appendix A.3.2 |
| MSR_IA32_VMX_EXIT_CTLS | 0x483 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU; in reference set only via the kvm_init_msr_lists VMX probe range | x86.c:7791 (kvm_init_msr_lists VMX probe range); x86.h:94–95; msr-index.h (0x483); Intel SDM Vol.3D Appendix A.4 |
| MSR_IA32_VMX_ENTRY_CTLS | 0x484 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU; in reference set only via the kvm_init_msr_lists VMX probe range | x86.c:7791 (kvm_init_msr_lists VMX probe range); x86.h:94–95; msr-index.h (0x484); Intel SDM Vol.3D Appendix A.5 |
| MSR_IA32_VMX_MISC | 0x485 | deny-gp | deny-gp | CPUID stability + timer devices: VMX hidden; additionally carries the VMX preemption-timer rate (a host-TSC-derived timing parameter), which must never reach the guest | x86.c:450 (emulated_msrs_all), x86.c:7791; msr-index.h (0x485); Intel SDM Vol.3D Appendix A.6 |
| MSR_IA32_VMX_CR0_FIXED0 | 0x486 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:451 (emulated_msrs_all), x86.c:7791; msr-index.h (0x486); Intel SDM Vol.3D Appendix A.7 |
| MSR_IA32_VMX_CR0_FIXED1 | 0x487 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU; in reference set only via the kvm_init_msr_lists VMX probe range | x86.c:7791 (kvm_init_msr_lists VMX probe range); x86.h:94–95; msr-index.h (0x487); Intel SDM Vol.3D Appendix A.7 |
| MSR_IA32_VMX_CR4_FIXED0 | 0x488 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:452 (emulated_msrs_all), x86.c:7791; msr-index.h (0x488); Intel SDM Vol.3D Appendix A.8 |
| MSR_IA32_VMX_CR4_FIXED1 | 0x489 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU; in reference set only via the kvm_init_msr_lists VMX probe range | x86.c:7791 (kvm_init_msr_lists VMX probe range); x86.h:94–95; msr-index.h (0x489); Intel SDM Vol.3D Appendix A.8 |
| MSR_IA32_VMX_VMCS_ENUM | 0x48a | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:453 (emulated_msrs_all), x86.c:7791; msr-index.h (0x48a); Intel SDM Vol.3D Appendix A.9 |
| MSR_IA32_VMX_PROCBASED_CTLS2 | 0x48b | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:454 (emulated_msrs_all), x86.c:7791; msr-index.h (0x48b); Intel SDM Vol.3D Appendix A.3.3 |
| MSR_IA32_VMX_EPT_VPID_CAP | 0x48c | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:455 (emulated_msrs_all), x86.c:7791; msr-index.h (0x48c); Intel SDM Vol.3D Appendix A.10 |
| MSR_IA32_VMX_TRUE_PINBASED_CTLS | 0x48d | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:446 (emulated_msrs_all), x86.c:7791; msr-index.h:1229; Intel SDM Vol.3D Appendix A.3.1 |
| MSR_IA32_VMX_TRUE_PROCBASED_CTLS | 0x48e | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:447 (emulated_msrs_all), x86.c:7791; msr-index.h:1230; Intel SDM Vol.3D Appendix A.3.2 |
| MSR_IA32_VMX_TRUE_EXIT_CTLS | 0x48f | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:448 (emulated_msrs_all), x86.c:7791; msr-index.h:1231; Intel SDM Vol.3D Appendix A.4 |
| MSR_IA32_VMX_TRUE_ENTRY_CTLS | 0x490 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:449 (emulated_msrs_all), x86.c:7791; msr-index.h:1232; Intel SDM Vol.3D Appendix A.5 |
| MSR_IA32_VMX_VMFUNC | 0x491 | deny-gp | deny-gp | CPUID stability: VMX hidden in frozen model; #GP architectural on VMX-less CPU | x86.c:456 (emulated_msrs_all), x86.c:7791; msr-index.h:1233; x86.h:95; Intel SDM Vol.3D Appendix A.11 |

[question] MSR_PLATFORM_INFO (0xce): the frozen max non-turbo ratio in bits 15:8 must equal
the frozen TSC frequency chosen by the CPUID 0x15/0x16 rows of the CPUID-model fragment
(ratio = frozen-TSC-Hz / 100 MHz). The disposition (allow-fixed read / deny-gp write) is
decided; the concrete numeric constant must be filled in at fragment-merge time from the
CPUID model's frozen frequency and then hashed into the versioned baseline per contract §6.

[question] MSR_CORE_THREAD_COUNT (0x35) is absent from arch/x86/include/asm/msr-index.h at
the pinned v6.18.35 tag (it is SDM-documented but model-specific), so it falls outside the
mechanically-checkable reference-set definition in task 06 §3 (KVM arrays + §7 names +
msr-index.h-matched classes). The row is kept with a safe allow-fixed(0x0001_0001) /
deny-gp disposition; confirm at merge whether it stays in the contract (recommended — the
guest may probe it on the frozen Intel model) or is dropped to keep the reference set
strictly mechanical.
