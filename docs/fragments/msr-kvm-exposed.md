> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

# MSR disposition fragment: class `kvm-exposed`

Class `kvm-exposed` covers the synthetic MSRs that KVM itself manufactures and advertises
through `KVM_GET_MSR_INDEX_LIST` — reference-set clause (a), the `emulated_msrs_all` array
in `arch/x86/kvm/x86.c` at v6.18.35 (lines 394–461; the pin agrees with
`guest/linux/versions.lock`, KERNEL_VERSION=6.18.35). Match rule: every `HV_X64_MSR_*`
name in that array's `CONFIG_KVM_HYPERV` block (x86.c:398–416) plus every `MSR_KVM_*`
PV-feature MSR in the array (x86.c:418–419, 459), excluding entries already disposed in
sibling fragments — the kvmclock fragment owns `MSR_KVM_WALL_CLOCK`/`MSR_KVM_SYSTEM_TIME`
and their `_NEW` variants (x86.c:395–396), and the `tsc` fragment owns
`HV_X64_MSR_TSC_FREQUENCY`/`APIC_FREQUENCY`/`REENLIGHTENMENT_CONTROL`/
`TSC_EMULATION_CONTROL`/`TSC_EMULATION_STATUS`/`TSC_INVARIANT_CONTROL` (x86.c:400–401,
410–411). None of these MSRs exist on real hardware: each is a door into a paravirtual
interface, and §7's kvmclock vector mandates that the frozen CPUID model hide the PV
leaves (`0x4000_00xx`) entirely. With neither the Hyper-V vendor leaves nor the KVM
signature leaf enumerated, every MSR in this class is architecturally nonexistent, so
`deny-gp` in both directions is bit-exact with the advertised CPU; correspondingly,
KVM_FEATURE_ASYNC_PF(4), KVM_FEATURE_STEAL_TIME(5), KVM_FEATURE_PV_EOI(6),
KVM_FEATURE_POLL_CONTROL(12), and KVM_FEATURE_ASYNC_PF_INT(14) are never reported (gate 5:
no half-exposed features). The determinism stakes are concrete: Hyper-V stimers are armed
as host hrtimers against the host-real-time reference counter
(`arch/x86/kvm/hyperv.c:634–682`), the VP-assist and SynIC pages are guest memory the host
mutates asynchronously, the syndbg MSRs are a network-backed debug transport, and the
`MSR_KVM_*` block leaks host scheduling and paging latency (async-PF delivery, steal
time, PV-EOI/poll-control interrupt-path coupling). Per the contract's §1 policy,
`deny-gp` here means: `KVM_X86_SET_MSR_FILTER` + `KVM_MSR_EXIT_REASON_FILTER` exit to
userspace, log MSR index and guest RIP, then inject #GP — never a silent in-kernel fault.
As defense in depth, vmm-core must not enable `KVM_CAP_HYPERV_*` capabilities, so KVM's
in-kernel Hyper-V emulation is never reachable even if the filter were misconfigured.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| HV_X64_MSR_GUEST_OS_ID | 0x40000000 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized to all PV enlightenments: writing the guest OS ID is the TLFS prerequisite for enabling the hypercall page and with it the whole Hyper-V time surface; no 0x4000_00xx leaves are enumerated, so the MSR does not exist — #GP architectural. | linux-6.18.35 arch/x86/kvm/x86.c:399 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:63; Hyper-V TLFS (guest OS identity MSR); INTEGRATION.md §7 (KVM paravirtual clock, CPUID stability) |
| HV_X64_MSR_HYPERCALL | 0x40000001 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": enabling it maps a host-supplied hypercall code page into guest memory (run-dependent guest-memory mutation) and opens the HvCall* surface, including timing hypercalls; interface hidden — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:399 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:64; Hyper-V TLFS (hypercall interface establishment); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_VP_INDEX | 0x40000002 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: the VP index is host-assigned topology surface with no architectural analog; interface hidden — #GP (single-vCPU contract has no legitimate consumer anyway). | linux-6.18.35 arch/x86/kvm/x86.c:405 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:65; Hyper-V TLFS (virtual processor index); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_RESET | 0x40000003 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: writing 1 triggers an immediate host-side partition reset — a guest-reachable host action outside the deterministic run loop and snapshot protocol; interface hidden — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:404 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:66; Hyper-V TLFS (HV_X64_MSR_RESET); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_VP_ASSIST_PAGE | 0x40000073 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock": enabling it designates a guest page the host writes enlightenment state into asynchronously (APIC assist, enlightened VMCS) — DMA-like run-dependent memory mutation; interface hidden — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:409 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:77, 145–148 (enable/address layout); Hyper-V TLFS (VP assist page); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SCONTROL | 0x40000080 | deny-gp | deny-gp | Closes §7 "Timer devices": SCONTROL enables the SynIC, gateway to SINTx/SIEFP/SIMP message and event pages that the host writes asynchronously and through which host-hrtimer-fired stimer expirations are delivered; interface hidden — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:407 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:80; Hyper-V TLFS (SynIC); INTEGRATION.md §7 (Timer devices, KVM paravirtual clock) |
| HV_X64_MSR_STIMER0_CONFIG | 0x400000b0 | deny-gp | deny-gp | Closes §7 "Timer devices" (no in-kernel timer unless proven V-time-driven): KVM arms stimers as host hrtimers against the host-real-time reference counter (hyperv.c stimer_start), the exact in-kernel-timer leak §7 forbids; guest timing goes through the architectural LAPIC rows backed by userspace TimerQueue instead — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:408 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.c:634–682 (stimer_start → hrtimer_start on get_time_ref_counter); include/hyperv/hvgdk_mini.h:114; INTEGRATION.md §7 (Timer devices) |
| HV_X64_MSR_CRASH_P0 | 0x40000100 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: crash parameter 0 of a guest→host notification channel that is host-side state, not vm_state; interface hidden — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:402 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:127; Linux Documentation/virt/kvm/api.rst (KVM_CAP_HYPERV); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_CRASH_P1 | 0x40000101 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: crash parameter 1, same hidden host-notification channel — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:402 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:128; INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_CRASH_P2 | 0x40000102 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: crash parameter 2, same hidden host-notification channel — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:402 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:129; INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_CRASH_P3 | 0x40000103 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: crash parameter 3, same hidden host-notification channel — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:403 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:130; INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_CRASH_P4 | 0x40000104 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: crash parameter 4, same hidden host-notification channel — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:403 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:131; INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_CRASH_CTL | 0x40000105 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: a read reports host crash-notify capability (CRASH_NOTIFY) — host policy, not guest state — and a write fires a host-side notification; interface hidden — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:403 (emulated_msrs_all, CONFIG_KVM_HYPERV); include/hyperv/hvgdk_mini.h:132, 140 (crash param count); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SYNDBG_OPTIONS | 0x400000ff | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: configures the Hyper-V synthetic debugger, a network-backed transport that imports external real-world I/O into the guest; syndbg CPUID leaves (0x40000080–82) never enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:412 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.h:54 (index 0x400000FF), 38–40 (syndbg CPUID leaves); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SYNDBG_CONTROL | 0x400000f1 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: send/receive control of the syndbg network transport — guest-triggered external I/O; never enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:413 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.h:49 (index 0x400000F1); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SYNDBG_STATUS | 0x400000f2 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: status readback reflects external debugger/network state, varying run to run; never enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:413 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.h:50 (index 0x400000F2); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SYNDBG_SEND_BUFFER | 0x400000f3 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: designates a guest page whose contents are pushed out the debug transport — guest-reachable external output; never enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:414 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.h:51 (index 0x400000F3); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SYNDBG_RECV_BUFFER | 0x400000f4 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: designates a guest page the host fills with received debug-network data — nondeterministic external input written into guest memory; never enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:414 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.h:52 (index 0x400000F4); INTEGRATION.md §7 (KVM paravirtual clock) |
| HV_X64_MSR_SYNDBG_PENDING_BUFFER | 0x400000f5 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" generalized: pending-buffer readback varies with external debugger traffic timing — a run-dependent read; never enumerated — #GP. | linux-6.18.35 arch/x86/kvm/x86.c:415 (emulated_msrs_all, CONFIG_KVM_HYPERV); arch/x86/kvm/hyperv.h:53 (index 0x400000F5); INTEGRATION.md §7 (KVM paravirtual clock) |
| MSR_KVM_ASYNC_PF_EN / MSR_KVM_STEAL_TIME / MSR_KVM_PV_EOI_EN / MSR_KVM_POLL_CONTROL / MSR_KVM_ASYNC_PF_INT / MSR_KVM_ASYNC_PF_ACK | 0x4b564d02–0x4b564d07 | deny-gp | deny-gp | Closes §7 "KVM paravirtual clock" (the MSR_KVM_* class): each leaks host scheduling/paging into the guest — async-PF delivery tracks host page-fault latency, steal time reports host preemption, PV-EOI and poll control re-route interrupt paths on host state; KVM signature leaf hidden and KVM_FEATURE_ASYNC_PF(4)/STEAL_TIME(5)/PV_EOI(6)/POLL_CONTROL(12)/ASYNC_PF_INT(14) never reported, so #GP is architectural (gate 5). | linux-6.18.35 arch/x86/kvm/x86.c:418–419, 459 (emulated_msrs_all); arch/x86/include/uapi/asm/kvm_para.h:26–35 (feature bits), 54–59 (indices); Linux Documentation/virt/kvm/x86/msr.rst; INTEGRATION.md §7 (KVM paravirtual clock) |

[question] emulated_msrs_all at v6.18.35 also lists HV_X64_MSR_TIME_REF_COUNT (0x40000020),
HV_X64_MSR_REFERENCE_TSC (0x40000021) (x86.c:400) and HV_X64_MSR_VP_RUNTIME (0x40000010)
(x86.c:406), which were not assigned to this fragment and are not in the `tsc` fragment —
confirm the kvmclock (or another sibling) fragment carries them; all three must be deny-gp
both directions (TIME_REF_COUNT/REFERENCE_TSC are direct host-real-time clocks, VP_RUNTIME
is host scheduling time, the Hyper-V analog of steal time), otherwise reference-set clause
(a) coverage has a gap.
