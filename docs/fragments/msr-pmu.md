> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

### MSR class: `pmu`

The host owns the PMU, non-negotiably, because the PMU *is* the V-time instrument: vmm-core programs a guest-only retired-branch perf_event counter and uses PMC overflow plus single-step to land injections at exact V-times (docs/PLAN.md Phase 2 and trap table "RDPMC → trap"; docs/RESEARCH.md rr and XenTT rows; antithesis.com/blog/deterministic_hypervisor/). No vPMU is exposed to the guest, closing the INTEGRATION.md §7 "PMU" leak vector: CPUID leaf 0xA reports architectural perfmon version 0, CPUID.1:ECX[15] (PDCM) is hidden, and RDPMC exits via VMX RDPMC-exiting and is answered with #GP (see the instruction-disposition table). Consequently **every MSR in this class is `deny-gp` for both reads and writes**: a denied access exits to userspace via `KVM_X86_SET_MSR_FILTER` + `KVM_CAP_X86_USER_SPACE_MSR` (`KVM_MSR_EXIT_REASON_FILTER`), is logged with MSR index and RIP, and only then is #GP injected — never a silent passthrough or silent zero. This is also architecturally consistent: with perfmon version 0 and PDCM clear, real hardware #GPs on these accesses too. Class match rule (mechanically checkable, all kernel citations at Linux tag v6.18.35): the union of (a) every entry of `arch/x86/kvm/x86.c:msrs_to_save_pmu`, and (b) every name in `arch/x86/include/asm/msr-index.h` or `arch/x86/include/asm/perf_event.h` matching `MSR_CORE_PERF_*`, `MSR_ARCH_PERFMON_*`, `MSR_IA32_PMC0`..`MSR_IA32_PMC7`, `MSR_IA32_PMC_V6_*`, `MSR_PEBS_*`, `MSR_IA32_PEBS_ENABLE`, `MSR_IA32_DS_AREA`, `MSR_OFFCORE_RSP_*`, `MSR_RELOAD_*`, `MSR_K7_EVNTSEL*`, `MSR_K7_PERFCTR*`, `MSR_F15H_PERF_*`, or `MSR_AMD64_PERF_CNTR_GLOBAL_*`, plus the exact-name additions `MSR_PERF_METRICS` and `MSR_IA32_PERF_CAPABILITIES`, plus the SDM architectural ranges given as range rows below (range rows deliberately overlap the per-name rows from the KVM array; dispositions are identical, so the overlap is harmless redundancy that lets either source be walked independently). Note: IA32_PERF_GLOBAL_STATUS_SET (0x391) and IA32_PERF_GLOBAL_INUSE (0x392) exist architecturally but have no `msr-index.h` define at this tag (only the AMD equivalent 0xc0000303 appears); the contract's default-deny catch-all covers them, and they are called out in the 0x38E–0x390 range row. Column grammar: `Read`/`Write` are drawn verbatim from the task-06 §3 disposition vocabulary; `Rationale` is one line beginning with the §7 leak vector it closes (`§7 PMU`).

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_ARCH_PERFMON_FIXED_CTR0 | 0x309 | deny-gp | deny-gp | §7 PMU: fixed counter 0 (instructions retired) counts real uarch progress incl. host noise; host owns PMU | kvm/x86.c:msrs_to_save_pmu; msr-index.h (MSR_CORE_PERF_FIXED_CTR0); antithesis.com/blog/deterministic_hypervisor/; docs/RESEARCH.md XenTT row |
| MSR_ARCH_PERFMON_FIXED_CTR1 | 0x30a | deny-gp | deny-gp | §7 PMU: fixed counter 1 (core cycles) leaks real frequency/time | kvm/x86.c:msrs_to_save_pmu; msr-index.h (MSR_CORE_PERF_FIXED_CTR1) |
| MSR_ARCH_PERFMON_FIXED_CTR0+2 | 0x30b | deny-gp | deny-gp | §7 PMU: fixed counter 2 (ref cycles) leaks wall-clock time directly; written as FIXED_CTR0+2 in the KVM array | kvm/x86.c:msrs_to_save_pmu; msr-index.h (MSR_CORE_PERF_FIXED_CTR2) |
| MSR_CORE_PERF_FIXED_CTR_CTRL | 0x38d | deny-gp | deny-gp | §7 PMU: fixed-counter control; a guest arm/disarm would contend with the host-owned PMU | kvm/x86.c:msrs_to_save_pmu; msr-index.h; SDM Vol3B 20.2.2 |
| MSR_CORE_PERF_GLOBAL_STATUS | 0x38e | deny-gp | deny-gp | §7 PMU: global status; overflow bits reflect real-time PMI timing of the host V-time counter | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_CORE_PERF_GLOBAL_CTRL | 0x38f | deny-gp | deny-gp | §7 PMU: global enable; the host V-time engine alone programs the PMU | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_IA32_PEBS_ENABLE | 0x3f1 | deny-gp | deny-gp | §7 PMU: PEBS enable; PEBS writes sample records to memory on real-time triggers — nondeterministic memory contents | kvm/x86.c:msrs_to_save_pmu; msr-index.h; SDM Vol3B 21.4 |
| MSR_IA32_DS_AREA | 0x600 | deny-gp | deny-gp | §7 PMU: debug-store area base; BTS/PEBS would scribble guest memory asynchronously to guest work | kvm/x86.c:msrs_to_save_pmu; msr-index.h; SDM Vol3B 17.4.9 / ch21 |
| MSR_PEBS_DATA_CFG | 0x3f2 | deny-gp | deny-gp | §7 PMU: PEBS data config; PEBS denied wholesale with the rest of the host-owned PMU | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_ARCH_PERFMON_PERFCTR0 | 0xc1 | deny-gp | deny-gp | §7 PMU: GP counter 0; cycle/event counts destroy determinism if guest-readable | kvm/x86.c:msrs_to_save_pmu; asm/perf_event.h |
| MSR_ARCH_PERFMON_PERFCTR1 | 0xc2 | deny-gp | deny-gp | §7 PMU: GP counter 1; same as PERFCTR0 | kvm/x86.c:msrs_to_save_pmu; asm/perf_event.h |
| MSR_ARCH_PERFMON_PERFCTR0+2 | 0xc3 | deny-gp | deny-gp | §7 PMU: GP counter 2 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_PERFCTR0+3 | 0xc4 | deny-gp | deny-gp | §7 PMU: GP counter 3 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_PERFCTR0+4 | 0xc5 | deny-gp | deny-gp | §7 PMU: GP counter 4 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_PERFCTR0+5 | 0xc6 | deny-gp | deny-gp | §7 PMU: GP counter 5 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_PERFCTR0+6 | 0xc7 | deny-gp | deny-gp | §7 PMU: GP counter 6 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_PERFCTR0+7 | 0xc8 | deny-gp | deny-gp | §7 PMU: GP counter 7; matches KVM_MAX_NR_INTEL_GP_COUNTERS | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_EVENTSEL0 | 0x186 | deny-gp | deny-gp | §7 PMU: event select 0; arming a counter would contend with the V-time retired-branch counter (cf. rr's IN_TX/IN_TXCP eventsel handling) | kvm/x86.c:msrs_to_save_pmu; asm/perf_event.h; rr src/PerfCounters.cc (~355-390, ~1127-1190) |
| MSR_ARCH_PERFMON_EVENTSEL1 | 0x187 | deny-gp | deny-gp | §7 PMU: event select 1 | kvm/x86.c:msrs_to_save_pmu; asm/perf_event.h |
| MSR_ARCH_PERFMON_EVENTSEL0+2 | 0x188 | deny-gp | deny-gp | §7 PMU: event select 2 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_EVENTSEL0+3 | 0x189 | deny-gp | deny-gp | §7 PMU: event select 3 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_EVENTSEL0+4 | 0x18a | deny-gp | deny-gp | §7 PMU: event select 4 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_EVENTSEL0+5 | 0x18b | deny-gp | deny-gp | §7 PMU: event select 5 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_EVENTSEL0+6 | 0x18c | deny-gp | deny-gp | §7 PMU: event select 6 | kvm/x86.c:msrs_to_save_pmu |
| MSR_ARCH_PERFMON_EVENTSEL0+7 | 0x18d | deny-gp | deny-gp | §7 PMU: event select 7 | kvm/x86.c:msrs_to_save_pmu |
| MSR_K7_EVNTSEL0 | 0xc0010000 | deny-gp | deny-gp | §7 PMU: AMD K7 event select 0; baseline is Intel-only (docs/PLAN.md Decision 0), AMD PMU never exposed | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_K7_EVNTSEL1 | 0xc0010001 | deny-gp | deny-gp | §7 PMU: AMD K7 event select 1; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_K7_EVNTSEL2 | 0xc0010002 | deny-gp | deny-gp | §7 PMU: AMD K7 event select 2; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_K7_EVNTSEL3 | 0xc0010003 | deny-gp | deny-gp | §7 PMU: AMD K7 event select 3; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_K7_PERFCTR0 | 0xc0010004 | deny-gp | deny-gp | §7 PMU: AMD K7 perf counter 0; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_K7_PERFCTR1 | 0xc0010005 | deny-gp | deny-gp | §7 PMU: AMD K7 perf counter 1; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_K7_PERFCTR2 | 0xc0010006 | deny-gp | deny-gp | §7 PMU: AMD K7 perf counter 2; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_K7_PERFCTR3 | 0xc0010007 | deny-gp | deny-gp | §7 PMU: AMD K7 perf counter 3; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTL0 | 0xc0010200 | deny-gp | deny-gp | §7 PMU: AMD F15h perf control 0; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_F15H_PERF_CTL1 | 0xc0010202 | deny-gp | deny-gp | §7 PMU: AMD F15h perf control 1 (stride 2); Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTL2 | 0xc0010204 | deny-gp | deny-gp | §7 PMU: AMD F15h perf control 2; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTL3 | 0xc0010206 | deny-gp | deny-gp | §7 PMU: AMD F15h perf control 3; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTL4 | 0xc0010208 | deny-gp | deny-gp | §7 PMU: AMD F15h perf control 4; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTL5 | 0xc001020a | deny-gp | deny-gp | §7 PMU: AMD F15h perf control 5; matches KVM_MAX_NR_AMD_GP_COUNTERS; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTR0 | 0xc0010201 | deny-gp | deny-gp | §7 PMU: AMD F15h perf counter 0; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_F15H_PERF_CTR1 | 0xc0010203 | deny-gp | deny-gp | §7 PMU: AMD F15h perf counter 1; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTR2 | 0xc0010205 | deny-gp | deny-gp | §7 PMU: AMD F15h perf counter 2; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTR3 | 0xc0010207 | deny-gp | deny-gp | §7 PMU: AMD F15h perf counter 3; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTR4 | 0xc0010209 | deny-gp | deny-gp | §7 PMU: AMD F15h perf counter 4; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_F15H_PERF_CTR5 | 0xc001020b | deny-gp | deny-gp | §7 PMU: AMD F15h perf counter 5; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu |
| MSR_AMD64_PERF_CNTR_GLOBAL_CTL | 0xc0000301 | deny-gp | deny-gp | §7 PMU: AMD PerfMonV2 global control; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_AMD64_PERF_CNTR_GLOBAL_STATUS | 0xc0000300 | deny-gp | deny-gp | §7 PMU: AMD PerfMonV2 global status; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_AMD64_PERF_CNTR_GLOBAL_STATUS_CLR | 0xc0000302 | deny-gp | deny-gp | §7 PMU: AMD PerfMonV2 global status clear; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_AMD64_PERF_CNTR_GLOBAL_STATUS_SET | 0xc0000303 | deny-gp | deny-gp | §7 PMU: AMD PerfMonV2 global status set; Intel-only baseline | kvm/x86.c:msrs_to_save_pmu; msr-index.h |
| MSR_IA32_PERF_CAPABILITIES | 0x345 | deny-gp | deny-gp | §7 PMU: enumerates LBR format/PEBS capability; PDCM (CPUID.1:ECX[15]) is hidden, so #GP on read is architecturally consistent; the MSR is read-only so writes #GP on real hardware too | kvm/x86.c:emulated_msrs_all + msr_based_features_all_except_vmx; msr-index.h; SDM Vol3B ch20; SDM Vol4 Table 2-2; KVM api.rst KVM_GET_MSR_FEATURE_INDEX_LIST (kernel.org) |
| MSR_OFFCORE_RSP_0 | 0x1a6 | deny-gp | deny-gp | §7 PMU: offcore-response aux event config for the host-owned PMU | msr-index.h |
| MSR_OFFCORE_RSP_1 | 0x1a7 | deny-gp | deny-gp | §7 PMU: offcore-response aux event config for the host-owned PMU | msr-index.h |
| MSR_CORE_PERF_FIXED_CTR3 | 0x30c | deny-gp | deny-gp | §7 PMU: fixed counter 3 (topdown slots); real-slot counts leak time | msr-index.h |
| MSR_PERF_METRICS | 0x329 | deny-gp | deny-gp | §7 PMU: topdown metrics derived from real slot counts; matched by exact-name addition to the class rule | msr-index.h |
| MSR_CORE_PERF_GLOBAL_OVF_CTRL | 0x390 | deny-gp | deny-gp | §7 PMU: IA32_PERF_GLOBAL_OVF_CTRL / GLOBAL_STATUS_RESET; note IA32_PERF_GLOBAL_STATUS_SET 0x391 and GLOBAL_INUSE 0x392 are architectural but NOT defined in msr-index.h at this tag (only AMD 0xc0000303 appears) — the default-deny catch-all still covers 0x391/0x392 | msr-index.h; SDM Vol3B 20.2.4 |
| MSR_PEBS_LD_LAT_THRESHOLD | 0x3f6 | deny-gp | deny-gp | §7 PMU: PEBS load-latency threshold; PEBS denied wholesale | msr-index.h |
| MSR_PEBS_FRONTEND | 0x3f7 | deny-gp | deny-gp | §7 PMU: PEBS frontend event config; PEBS denied wholesale | msr-index.h |
| MSR_IA32_PMC0 | 0x4c1 | deny-gp | deny-gp | §7 PMU: full-width counter alias (IA32_A_PMC0); range extends 0x4c1+N — see 0x4C1-0x4C8 range row | msr-index.h |
| MSR_RELOAD_FIXED_CTR0 | 0x1309 | deny-gp | deny-gp | §7 PMU: adaptive-PEBS reload base for fixed counters | msr-index.h |
| MSR_RELOAD_PMC0 | 0x14c1 | deny-gp | deny-gp | §7 PMU: adaptive-PEBS reload base for GP counters | msr-index.h |
| MSR_IA32_PMC_V6_GP0_CTR | 0x1900 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 GP counter base; per-counter stride MSR_IA32_PMC_V6_STEP=4, so the whole 0x1900+4N bank is denied by the catch-all | msr-index.h |
| MSR_IA32_PMC_V6_GP0_CFG_A | 0x1901 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 GP config A (stride 4) | msr-index.h |
| MSR_IA32_PMC_V6_GP0_CFG_B | 0x1902 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 GP config B (stride 4) | msr-index.h |
| MSR_IA32_PMC_V6_GP0_CFG_C | 0x1903 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 GP config C (stride 4) | msr-index.h |
| MSR_IA32_PMC_V6_FX0_CTR | 0x1980 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 fixed counter base (stride 4) | msr-index.h |
| MSR_IA32_PMC_V6_FX0_CFG_B | 0x1982 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 fixed config B (stride 4) | msr-index.h |
| MSR_IA32_PMC_V6_FX0_CFG_C | 0x1983 | deny-gp | deny-gp | §7 PMU: arch-perfmon v6 fixed config C (stride 4) | msr-index.h |
| IA32_PMC0-7 | 0xC1-0xC8 | deny-gp | deny-gp | §7 PMU: general-purpose performance counters; cycle/event/uarch counts that destroy determinism if guest-readable — host owns PMU | SDM Vol3B ch20; SDM Vol4 Table 2-2 |
| IA32_A_PMC0-7 | 0x4C1-0x4C8 | deny-gp | deny-gp | §7 PMU: full-width aliases of the GP PMCs (when PERF_CAPABILITIES.FW_WRITE); same nondeterminism | SDM Vol3B 20.2.4 |
| IA32_PERFEVTSEL0-7 | 0x186-0x18D | deny-gp | deny-gp | §7 PMU: event-select MSRs that arm the GP PMCs; denied along with the counters | SDM Vol3B ch20; SDM Vol4 Table 2-2 |
| IA32_FIXED_CTR0-2 | 0x309-0x30B | deny-gp | deny-gp | §7 PMU: fixed-function counters (instr retired, core cycles, ref cycles); REF_CYCLES especially leaks real time | SDM Vol3B 20.2.2 |
| IA32_PERF_GLOBAL_STATUS/CTRL/OVF_CTRL | 0x38E-0x390 | deny-gp | deny-gp | §7 PMU: global PMU enable/overflow/status; the host's V-time engine alone programs the PMU; adjacent architectural 0x391 (GLOBAL_STATUS_SET) and 0x392 (GLOBAL_INUSE) are covered by the default-deny catch-all | SDM Vol3B 20.2.4 |
| MSR_UNC_PERF_FIXED_CTRL/CTR, MSR_UNC_CBO_CONFIG | 0x394-0x396 | deny-gp | deny-gp | §7 PMU: client uncore fixed counter control/counter and CBo config; uncore counts cross-core and host activity — pure nondeterminism; names not in msr-index.h at this tag (defined in arch/x86/events/intel/uncore_snb.c), covered here and by the catch-all | SDM Vol4 Table 2-2; linux arch/x86/events/intel/uncore_snb.c |
