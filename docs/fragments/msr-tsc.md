> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

# MSR disposition fragment: class `tsc`

Class `tsc` covers every MSR through which the time-stamp counter — the primary time leak
named in INTEGRATION.md §7 — can carry host real time into the guest: the counter itself
(`IA32_TSC`), its software offset (`IA32_TSC_ADJUST`), the RDTSCP/RDPID auxiliary value
(`IA32_TSC_AUX`), the TSC-deadline LAPIC timer arm register, AMD's TSC scaling ratio, and
the six Hyper-V synthetic MSRs that re-export TSC/APIC frequency and TSC-emulation
machinery. The governing rule is §7's TSC-plumbing clause: the host TSC must never reach
the guest. Every readable value in this class is therefore either derived from
`consonance/vtime` (`VClock::tsc(work) = tsc_base + floor(vns(work) · tsc_hz / 10⁹)`, with
`tsc_base`/ratio captured in the `vm_state` blob per §4) or echoed from guest-written
state held in `vm_state`; timer arming goes through the userspace `TimerQueue` (§7 "Timer
devices": no in-kernel LAPIC hrtimer, which runs on host real time); and everything not
derivable from V-time — in particular all Hyper-V enlightenments, which the frozen CPUID
model does not advertise — is default-deny (#GP) under `KVM_X86_SET_MSR_FILTER`, surfaced
as a loud event rather than a passthrough.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| MSR_IA32_TSC | 0x10 | emulate-vtime | emulate-vtime | Closes §7 "TSC plumbing": reads return VClock::tsc(work) computed from retired-branch work, never the host counter; a write deterministically rebases tsc_base in vm_state (new_base = value − floor(vns·tsc_hz/10⁹)) so readback is coherent and replayable. | linux-6.18.35 arch/x86/kvm/x86.c (msrs_to_save_base); arch/x86/include/asm/msr-index.h; Intel SDM Vol.4 Table 2-2; INTEGRATION.md §7 (TSC plumbing) + §4 (vm_state); consonance/vtime/src/clock.rs (VClock::tsc); kernel.org KVM x86 timekeeping doc |
| MSR_TSC_AUX | 0xc0000103 | allow-stateful | allow-stateful | Closes §7 "TSC plumbing" (rr-paper current-core leak, arXiv:1610.02144): RDTSCP/RDPID aux must echo the guest-written value held in vm_state, never the host's per-core IA32_TSC_AUX; pure software state with no time content of its own. | linux-6.18.35 arch/x86/kvm/x86.c (msrs_to_save_base); arch/x86/include/asm/msr-index.h; Intel SDM Vol.4 Table 2-2 + RDTSCP/RDPID (felixcloutier.com/x86/rdpid); INTEGRATION.md §4 (vm_state MSR capture) |
| HV_X64_MSR_TSC_FREQUENCY | 0x40000022 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized to all PV time enlightenments: the frozen CPUID model exposes no Hyper-V leaves (no HV_ACCESS_FREQUENCY_MSRS), so this synthetic frequency MSR architecturally does not exist and a host-derived tsc_hz must not leak through it. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); Hyper-V TLFS (asm/hyperv-tlfs.h); INTEGRATION.md §7 (KVM paravirtual clock, CPUID stability) |
| HV_X64_MSR_APIC_FREQUENCY | 0x40000023 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" / "Timer devices": a host-derived APIC timer frequency would let the guest correlate V-time with real time; no Hyper-V leaves are enumerated, so the MSR does not exist — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); Hyper-V TLFS (asm/hyperv-tlfs.h); INTEGRATION.md §7 (KVM paravirtual clock, Timer devices) |
| HV_X64_MSR_REENLIGHTENMENT_CONTROL | 0x40000106 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": reenlightenment is migration-driven host-real-time TSC notification machinery with no deterministic analog; not enumerated by the frozen CPUID model — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); Hyper-V TLFS (asm/hyperv-tlfs.h); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_TSC_EMULATION_CONTROL | 0x40000107 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": Hyper-V TSC-emulation toggling would hand the guest a second, host-coupled TSC control plane beside VClock; not enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); Hyper-V TLFS (asm/hyperv-tlfs.h); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_TSC_EMULATION_STATUS | 0x40000108 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": emulation-status readback reflects host migration state, which is nondeterministic across runs; not enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); Hyper-V TLFS (asm/hyperv-tlfs.h); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_TSC_INVARIANT_CONTROL | 0x40000118 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": invariant-TSC enlightenment control is host TSC policy surface; the guest sees invariant TSC only via the frozen CPUID model, never via Hyper-V MSRs — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); Hyper-V TLFS (asm/hyperv-tlfs.h); INTEGRATION.md §7 (KVM paravirtual clock, CPUID stability) |
| MSR_IA32_TSC_ADJUST | 0x3b | emulate-vtime | emulate-vtime | Closes §7 "TSC plumbing": the SDM coherence rule (a write of delta to TSC_ADJUST also shifts IA32_TSC by delta) is satisfied entirely inside VClock — the adjust value and the rebased tsc_base both live in vm_state, with no host TSC involvement in either direction. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/asm/msr-index.h; Intel SDM Vol.3B §17.17.3 (TSC_ADJUST coherence) + Vol.4 Table 2-2; INTEGRATION.md §7 (TSC plumbing) + §4 (vm_state) |
| MSR_IA32_TSC_DEADLINE | 0x6e0 | deny-gp | deny-gp | (round-7: **deny-gp**, was emulate-timerqueue; authoritative row is spine §3.3) TSC-deadline hidden (CPUID.1:ECX[24]=0): the in-kernel WRMSR fastpath swallows a 0x6e0 write before the MSR filter under `KVM_IRQCHIP_NONE`, so emulate-timerqueue is unbacked; the LAPIC timer is xAPIC LVT one-shot/periodic (spine §5). Aligns with Ruling R1. | spine §3.3; linux-6.18.35 arch/x86/kvm/vmx/vmx.c (handle_fastpath_wrmsr), arch/x86/kvm/lapic.c (kvm_set_lapic_tscdeadline_msr no-op w/o apic); INTEGRATION.md §7 (Timer devices) |
| MSR_AMD64_TSC_RATIO | 0xc0000104 | deny-gp | deny-gp | Architectural, and closes §7 "TSC plumbing" (offset/scaling must never let host TSC reach the guest): TSC scaling is hypervisor-side machinery, only architecturally present when CPUID 8000_000AH EDX[4] (TscRateMsr) is set, which the frozen CPUID model does not set — #GP. | linux-6.18.35 arch/x86/kvm/x86.c (emulated_msrs_all); arch/x86/include/asm/msr-index.h; AMD APM Vol.2 §15.30.5 (TSC ratio MSR); INTEGRATION.md §7 (TSC plumbing, CPUID stability) |
