> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

## MSR class: `intel-pt` — Intel Processor Trace (`IA32_RTIT_*`)

Match rule: every name matching `MSR_IA32_RTIT_*` in `arch/x86/include/asm/msr-index.h`
at v6.18.35 (lines 330–380: indexes 0x560–0x561, 0x570–0x572, 0x580–0x587), plus the
architecturally reserved address-filter extension 0x588–0x58B (`ADDRn_A/B` for n=4,5; SDM
Vol3C §33.2.7 sizes the filter space by CPUID.(EAX=14H,ECX=1):EAX[2:0], and msr-index.h
names only n=0–3). All thirteen named MSRs appear in KVM's `msrs_to_save_base`
(`arch/x86/kvm/x86.c:330–345`), so they are in the reference set via
`KVM_GET_MSR_INDEX_LIST`. The entire class is denied in both directions: Intel PT is
hidden in the frozen CPUID model (CPUID.7,0:EBX[25]=0; leaf 0x14 zeroed), and on a CPU
without PT every `IA32_RTIT_*` access raises #GP — so `deny-gp` is bit-exact with the
advertised CPU. The determinism case is direct: an enabled trace embeds host-real-time
TSC/MTC/CYC timestamp packets (`RTIT_CTL` bits TSC_EN/MTC_EN/CYCLEACC, msr-index.h:332–341)
and streams them asynchronously into guest-visible memory at `OUTPUT_BASE`, a DMA-like
run-dependent memory mutation, while `RTIT_STATUS`'s PacketByteCnt/BUFFOVF fields
(msr-index.h:362–369) vary with trace volume — host TSC reaching the guest through a side
door, exactly §7's TSC-plumbing vector. Per the contract's §1 policy, `deny-gp` here means:
`KVM_X86_SET_MSR_FILTER` + `KVM_MSR_EXIT_REASON_FILTER` exit to userspace, log MSR index
and guest RIP, then inject #GP — never a silent in-kernel fault. KVM only probes these
MSRs when `X86_FEATURE_INTEL_PT` is reported (`x86.c:7682–7703`); since our CPUID model
never reports it, denying is also consistent with gate 5 (no half-exposed features).
The two range-style source entries (0x560–0x561, 0x580–0x58B) are folded into the named
rows below; 0x588–0x58B keeps its own row because msr-index.h has no names for it.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_IA32_RTIT_OUTPUT_BASE | 0x560 | deny-gp | deny-gp | Closes §7 TSC plumbing: steers async trace output (host-timed TSC/MTC/CYC packets) into guest memory; PT hidden in CPUID, #GP architectural | x86.c:341 (msrs_to_save_base), 7692; msr-index.h:379; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_OUTPUT_MASK | 0x561 | deny-gp | deny-gp | Closes §7 TSC plumbing: output mask/pointers for the same run-dependent trace buffer; PT hidden in CPUID, #GP architectural | x86.c:341 (msrs_to_save_base), 7693; msr-index.h:380; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_CTL | 0x570 | deny-gp | deny-gp | Closes §7 TSC plumbing: TraceEn/TSC_EN/MTC_EN/CYCLEACC would write host-real-time packets into guest memory; PT hidden in CPUID (7,0:EBX[25]=0) | x86.c:340 (msrs_to_save_base), 7682; msr-index.h:330; SDM Vol3C §33.2.7.2 |
| MSR_IA32_RTIT_STATUS | 0x571 | deny-gp | deny-gp | Closes §7 TSC plumbing: PacketByteCnt/BUFFOVF vary with host-timed trace volume — run-dependent reads; PT hidden in CPUID | x86.c:340 (msrs_to_save_base), 7683; msr-index.h:361; SDM Vol3C ch. 33 |
| MSR_IA32_RTIT_CR3_MATCH | 0x572 | deny-gp | deny-gp | Closes §7 TSC plumbing: CR3 filter only steers the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:340 (msrs_to_save_base), 7687; msr-index.h:378; SDM Vol3C ch. 33 |
| MSR_IA32_RTIT_ADDR0_A | 0x580 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range start for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:342 (msrs_to_save_base), 7699; msr-index.h:370; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR0_B | 0x581 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range end for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:342 (msrs_to_save_base), 7699; msr-index.h:371; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR1_A | 0x582 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range start for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:343 (msrs_to_save_base), 7699; msr-index.h:372; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR1_B | 0x583 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range end for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:343 (msrs_to_save_base), 7699; msr-index.h:373; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR2_A | 0x584 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range start for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:344 (msrs_to_save_base), 7699; msr-index.h:374; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR2_B | 0x585 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range end for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:344 (msrs_to_save_base), 7699; msr-index.h:375; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR3_A | 0x586 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range start for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:345 (msrs_to_save_base), 7699; msr-index.h:376; SDM Vol3C §33.2.7 |
| MSR_IA32_RTIT_ADDR3_B | 0x587 | deny-gp | deny-gp | Closes §7 TSC plumbing: IP-filter/TraceStop range end for the denied trace facility; PT hidden in CPUID, #GP architectural | x86.c:345 (msrs_to_save_base), 7699; msr-index.h:377; SDM Vol3C §33.2.7 |
| IA32_RTIT_ADDR4_A–ADDR5_B (reserved range) | 0x588–0x58B | deny-gp | deny-gp | Closes §7 TSC plumbing: reserved PT address-filter extension (n=4,5) beyond msr-index.h's named n=0–3; unimplemented MSR access #GPs architecturally | SDM Vol3C §33.2.7 (range count via CPUID.(EAX=14H,ECX=1):EAX[2:0]); libipt pt_config |
