> **NON-NORMATIVE — construction artifact.** This per-class fragment was used to
> assemble the contract and is **not** parsed or hashed. The authoritative, hashed
> surface is the spine `docs/CPU-MSR-CONTRACT.md` (§1–§6) plus `docs/cpu-msr-contract.toml`.
> Where this fragment and the spine/TOML disagree, **the spine and TOML win.**

# MSR disposition fragment: class `x2apic`

Class `x2apic` covers INTEGRATION.md §7's "x2APIC MSR surface" vector — defined exactly as
every MSR index `I` with `0x800 ≤ I ≤ 0x8FF` (the architecturally reserved x2APIC MSR
address space; register at `I = 0x800 + (xAPIC offset >> 4)`, `APIC_BASE_MSR` in
`arch/x86/kvm/lapic.c` at v6.18.35), with the defined-register subset taken from Intel SDM
Vol.4 Table 2-2 / x2APIC spec 318148 (802H–83FH) and cross-checked against
`kvm_lapic_readable_reg_mask` plus the write-only EOI/SELF-IPI cases in `lapic.c` at the
same tag; the rows below partition the full range with no gaps. The disposition is forced
by two documented pinned-kernel facts: (1) `Documentation/virt/kvm/api.rst` at v6.18.35 —
"Enabling x2APIC in KVM_SET_CPUID2 requires KVM_CREATE_IRQCHIP as KVM doesn't support
forwarding x2APIC MSR accesses to userspace"; and (2) the `KVM_X86_SET_MSR_FILTER` caveat —
"x2APIC MSR accesses cannot be filtered (KVM silently ignores filters that cover any x2APIC
MSRs)". So if x2APIC were exposed, every register in this block would be serviced by the
in-kernel LAPIC with no userspace interposition possible — and that LAPIC's timer is host
real time: TMCCT reads are computed from `ktime_get()`/hrtimer-remaining
(`lapic.c apic_get_tmcct`) and TMICT/LVT-timer writes arm host hrtimers whose expiry
injects interrupts at host-determined instants — exactly what §7 "Timer devices" forbids
("no KVM in-kernel timer devices unless proven V-time-driven") and what docs/PLAN.md's
interrupt-timing row forbids (interrupts only host-injected at exact V-time). The contract
therefore hides x2APIC: the frozen CPUID model clears CPUID.1:ECX[21], the guest's only
APIC is the userspace-emulated xAPIC MMIO page backed by `TimerQueue`/V-time (no
`KVM_CREATE_IRQCHIP`; per-register semantics in the rationales below; the MMIO sub-table
itself is contract section 5), and `IA32_APIC_BASE.EXTD` becomes a reserved bit
(`lapic.c kvm_apic_set_base` folds `X2APIC_ENABLE` into `reserved_bits` when guest CPUID
lacks X2APIC) — x2APIC mode can never be entered, so every RDMSR/WRMSR in 0x800–0x8FF takes
the architectural #GP (SDM: the block is accessible only when EXTD=1). The deny is still
loud despite the filter carve-out: with `KVM_MSR_EXIT_REASON_INVAL` enabled in
`KVM_CAP_X86_USER_SPACE_MSR`, the failed access exits as `KVM_EXIT_X86_RDMSR`/`WRMSR`
(reason INVAL, `x86.c kvm_msr_reason`), is logged with index and RIP, then completes with
`error = 1` so KVM injects #GP — never a silent zero. (round-7: **TSC-deadline is hidden**,
CPUID.1:ECX[24]=0, MSR 0x6E0 deny-gp — the old "exposable independently" claim was wrong:
api.rst permitting the CPUID bit does NOT make the WRMSR serviceable, because the in-kernel
WRMSR fastpath swallows 0x6E0 before the MSR filter under `KVM_IRQCHIP_NONE`; see spine §3.3.
The LAPIC timer is xAPIC LVT one-shot/periodic only.) The task-04 guest kernel drops
`CONFIG_X86_X2APIC` as defense in depth.

| MSR | Index | Read | Write | Rationale | Citation |
|---|---|---|---|---|---|
| IA32_X2APIC_APICID | 0x802 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": x2APIC hidden (CPUID.1:ECX[21]=0, APIC_BASE.EXTD reserved) so the MSR alias #GPs; the logical APIC ID is served at xAPIC MMIO 020H as frozen 0 — single-vCPU topology (docs/PLAN.md: one vCPU, period), no host topology leak. | Intel SDM Vol.4 Table 2-2 (802H) + Vol.3A ch.11; x2APIC spec 318148; linux-6.18.35 arch/x86/kvm/lapic.c (kvm_apic_set_base reserved_bits); INTEGRATION.md §7 (x2APIC MSR surface) |
| IA32_X2APIC_VERSION | 0x803 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; xAPIC 030H returns the frozen constant 0x00050014 (version 14H, max-LVT 5, no directed-EOI) from the userspace model — mirrors KVM's APIC_VERSION so no host APIC revision leaks and the LVT-CMCI row stays architecturally absent. | Intel SDM Vol.4 Table 2-2 (803H); linux-6.18.35 arch/x86/kvm/lapic.c (APIC_VERSION 0x14, kvm_apic_set_version); INTEGRATION.md §7 (x2APIC MSR surface, CPUID stability) |
| IA32_X2APIC_TPR | 0x808 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; TPR is pure guest-written priority state at xAPIC 080H in the userspace LAPIC, captured in vm_state (§4) — architectural state, no time content. | Intel SDM Vol.4 Table 2-2 (808H); INTEGRATION.md §7 (x2APIC MSR surface) + §4 (vm_state LAPIC state) |
| IA32_X2APIC_PPR | 0x80a | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs (architecturally read-only besides); xAPIC 0A0H PPR is computed deterministically as a pure function of captured TPR/ISR state — never a host-priority artifact. | Intel SDM Vol.4 Table 2-2 (80AH) + Vol.3A ch.11 (PPR computation); INTEGRATION.md §7 (x2APIC MSR surface) + §4 |
| IA32_X2APIC_EOI | 0x80b | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs (reads #GP even in x2APIC — write-only register); xAPIC 0B0H writes retire the highest in-service ISR bit in the userspace model — the single deterministic EOI path that the kvmclock fragment's MSR_KVM_PV_EOI_EN deny preserves. | Intel SDM Vol.4 Table 2-2 (80BH, WO); linux-6.18.35 arch/x86/kvm/lapic.c (kvm_lapic_readable_reg_mask: EOI not readable); INTEGRATION.md §7 (x2APIC MSR surface); fragments/msr-kvmclock.md (PV_EOI row) |
| IA32_X2APIC_LDR | 0x80d | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; xAPIC 0D0H LDR (with DFR) is guest-writable logical-destination state captured in vm_state — consulted only by deterministic userspace delivery, trivial on one vCPU. | Intel SDM Vol.4 Table 2-2 (80DH); INTEGRATION.md §7 (x2APIC MSR surface) + §4 |
| IA32_X2APIC_SIVR | 0x80f | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; the software-enable bit and spurious vector live in the userspace model (vm_state §4), and spurious-interrupt delivery occurs only at deterministic emulation points — never on a host-timed race. | Intel SDM Vol.4 Table 2-2 (80FH); INTEGRATION.md §7 (x2APIC MSR surface) + §4 |
| IA32_X2APIC_ISR0–ISR7 | 0x810-0x817 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": aliases #GP (read-only registers); in-service bitmaps at xAPIC 100H–170H are a pure function of InjectionPlanner injections (interrupts only host-injected at exact V-time — docs/PLAN.md interrupt-timing row) and guest EOIs, serialized as §4's pending/in-service interrupt state. | Intel SDM Vol.4 Table 2-2 (810H–817H); docs/PLAN.md (interrupt timing row); INTEGRATION.md §7 (x2APIC MSR surface) + §4; consonance/vtime/src/planner.rs (InjectionPlanner); antithesis.com/blog/deterministic_hypervisor/ (APIC delivery + virtual time) |
| IA32_X2APIC_TMR0–TMR7 | 0x818-0x81f | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": aliases #GP (read-only); trigger-mode bitmaps at xAPIC 180H–1F0H are set deterministically at userspace-IOAPIC delivery time and captured in vm_state — no host edge/level race exists. | Intel SDM Vol.4 Table 2-2 (818H–81FH); INTEGRATION.md §7 (x2APIC MSR surface, Timer devices) + §4 |
| IA32_X2APIC_IRR0–IRR7 | 0x820-0x827 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": aliases #GP (read-only); request bitmaps at xAPIC 200H–270H mutate only on planner-scheduled V-time injections and ICR self-IPIs — never on host-timed events, so polling IRR cannot observe real time. | Intel SDM Vol.4 Table 2-2 (820H–827H); docs/PLAN.md (interrupt timing row); INTEGRATION.md §7 (x2APIC MSR surface) + §4 |
| IA32_X2APIC_ESR | 0x828 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs (x2APIC additionally #GPs non-zero writes); xAPIC 280H follows the write-then-read protocol over error state generated only by deterministic emulation events (e.g. illegal-vector writes), never host conditions. | Intel SDM Vol.4 Table 2-2 (828H); linux-6.18.35 arch/x86/kvm/lapic.c (kvm_lapic_reg_write APIC_ESR x2apic non-zero reject); INTEGRATION.md §7 (x2APIC MSR surface) |
| IA32_X2APIC_LVT_CMCI | 0x82f | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface" plus the host-event family of §7 "PMU"/"Power/frequency": CMCI reports host-physical corrected-machine-check events — nondeterministic by nature; the frozen version value (max-LVT 5) makes even xAPIC 2F0H reserved (KVM analog: LVT_CMCI exists only with MCG_CMCI_P, which is never set). | Intel SDM Vol.4 Table 2-2 (82FH); linux-6.18.35 arch/x86/kvm/lapic.c (kvm_apic_calc_nr_lvt_entries: MCG_CMCI_P gate); INTEGRATION.md §7 (x2APIC MSR surface) |
| IA32_X2APIC_ICR | 0x830 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": the 64-bit x2APIC ICR is unreachable; xAPIC 300H/310H IPIs on the single vCPU are self/fixed-only, queued to IRR at deterministic emulation points with delivery-status always idle — interrupt arrival stays planner-controlled, never asynchronous. | Intel SDM Vol.4 Table 2-2 (830H); linux-6.18.35 arch/x86/kvm/lapic.c (kvm_x2apic_icr_write, X2APIC_ICR_RESERVED_BITS); kernel.org KVM errata + KVM_CAP_X2APIC_API (x2APIC ICR/dest-ID quirks — moot with x2APIC hidden); INTEGRATION.md §7 (x2APIC MSR surface); antithesis.com/blog/deterministic_hypervisor/ |
| IA32_X2APIC_LVT_TIMER | 0x832 | deny-gp | deny-gp | Closes §7 "Timer devices": alias #GPs; xAPIC 320H mode/mask/vector writes deterministically arm, rearm, or cancel the TimerQueue entry — **one-shot/periodic only; TSC-deadline mode (10b) unavailable** (CPUID.1:ECX[24]=0, MSR 0x6E0 deny-gp, spine §3.3) — never a KVM hrtimer on host real time. | Intel SDM Vol.3A ch.11 (LVT timer modes) + Vol.4 Table 2-2 (832H); linux-6.18.35 arch/x86/kvm/lapic.c (lapic_timer hrtimer — the avoided path); INTEGRATION.md §7 (Timer devices); spine §3.3 (0x6e0 deny-gp) |
| IA32_X2APIC_LVT_THERMAL | 0x833 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface" via §7 "Power/frequency": alias #GPs; xAPIC 330H is writable state in vm_state, but thermal events are host-physical and no thermal model exists, so the LVT never fires — programming it is inert and deterministic. | Intel SDM Vol.4 Table 2-2 (833H); INTEGRATION.md §7 (Power/frequency, x2APIC MSR surface) + §4 |
| IA32_X2APIC_LVT_PMI | 0x834 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface" via §7 "PMU": alias #GPs; xAPIC 340H is writable state in vm_state, but no vPMU is exposed (host owns the PMU; RDPMC traps), so no counter-overflow PMI can ever be generated toward the guest — the LVT never fires. | Intel SDM Vol.4 Table 2-2 (834H); INTEGRATION.md §7 (PMU, x2APIC MSR surface); docs/PLAN.md (RDPMC trap row) |
| IA32_X2APIC_LVT_LINT0 | 0x835 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; xAPIC 350H is the ExtINT wiring for the userspace PIC — every interrupt it can carry originates from TimerQueue-backed device models injected at exact V-time, never a physical pin. | Intel SDM Vol.4 Table 2-2 (835H); INTEGRATION.md §7 (Timer devices, x2APIC MSR surface) + §5 adapter map (PIT/PIC backed by TimerQueue) |
| IA32_X2APIC_LVT_LINT1 | 0x836 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; xAPIC 360H's NMI pin is never pulsed by any host event — an NMI, if ever used, is a planner decision at an exact V-time, not a watchdog (docs/PLAN.md guest config: no watchdogs). | Intel SDM Vol.4 Table 2-2 (836H); docs/PLAN.md (guest config: no watchdogs; interrupt timing row); INTEGRATION.md §7 (x2APIC MSR surface) |
| IA32_X2APIC_LVT_ERROR | 0x837 | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": alias #GPs; xAPIC 370H holds the vector for APIC errors that arise only from deterministic emulation (illegal vectors etc.), so error-interrupt timing is replayable by construction. | Intel SDM Vol.4 Table 2-2 (837H); INTEGRATION.md §7 (x2APIC MSR surface) + §4 |
| IA32_X2APIC_INIT_COUNT | 0x838 | deny-gp | deny-gp | Closes §7 "Timer devices" (the arming half): with the in-kernel LAPIC this write would arm a host hrtimer KVM fires on real time; alias dead, and the xAPIC 380H write converts count × divide × frozen bus period to an absolute V-ns deadline on the TimerQueue (0 disarms; periodic mode re-arms deterministically), read returning the stored initial count from vm_state. | Intel SDM Vol.3A ch.11 (timer) + Vol.4 Table 2-2 (838H); linux-6.18.35 arch/x86/kvm/lapic.c (lapic_timer hrtimer arming — the avoided path); INTEGRATION.md §7 (Timer devices) + §3 (idle-skip via TimerQueue::peek_next); consonance/vtime/src/queue.rs (TimerQueue) |
| IA32_X2APIC_CUR_COUNT | 0x839 | deny-gp | deny-gp | Closes §7 "Timer devices" — the countdown leak named for this class: KVM computes this read from ktime_get()/hrtimer-remaining (host real time); alias dead, and xAPIC 390H is served emulate-vtime: remaining = ticks((deadline_vns − VClock::vns(work)) / tick_vns), 0 when unarmed or expired — monotone in retired-branch work and bit-identical on replay. | linux-6.18.35 arch/x86/kvm/lapic.c (apic_get_tmcct: ktime_get); Intel SDM Vol.4 Table 2-2 (839H, RO); INTEGRATION.md §7 (Timer devices); consonance/vtime/src/clock.rs (VClock::vns); antithesis.com/blog/deterministic_hypervisor/ (virtual time) |
| IA32_X2APIC_DIV_CONF | 0x83e | deny-gp | deny-gp | Closes §7 "Timer devices": alias #GPs; the xAPIC 3E0H divide value (bits 0,1,3) is stored state in vm_state feeding the INIT_COUNT/CUR_COUNT tick conversions — a divide rewrite deterministically recomputes the armed TimerQueue deadline, with no hrtimer to cancel. | Intel SDM Vol.4 Table 2-2 (83EH); linux-6.18.35 arch/x86/kvm/lapic.c (APIC_TDCR write → hrtimer restart — the avoided path); INTEGRATION.md §7 (Timer devices) + §4 |
| IA32_X2APIC_SELF_IPI | 0x83f | deny-gp | deny-gp | Closes §7 "x2APIC MSR surface": SELF IPI is x2APIC-only with no xAPIC alias (offset 3F0H is reserved), so with x2APIC hidden the register exists nowhere — self-IPIs use the ICR path, which is deterministic per its row. | Intel SDM Vol.4 Table 2-2 (83FH, WO, x2APIC-only); linux-6.18.35 arch/x86/kvm/lapic.c (APIC_SELF_IPI rejected outside x2apic mode); INTEGRATION.md §7 (x2APIC MSR surface) |
| X2APIC reserved (defined-register gaps) | 0x800-0x801, 0x804-0x807, 0x809, 0x80c, 0x80e, 0x829-0x82e, 0x831, 0x83a-0x83d | deny-gp | deny-gp | Architectural: reserved x2APIC addresses #GP even in x2APIC mode (APR, RRD, DFR, and ICR2 have no x2APIC counterpart) — doubly dead here with EXTD unreachable; matches kvm_lapic_readable_reg_mask leaving these bits clear. | Intel SDM Vol.3A ch.11 (x2APIC address space, reserved entries) + x2APIC spec 318148; linux-6.18.35 arch/x86/kvm/lapic.c (kvm_lapic_readable_reg_mask: ARBPRI/DFR/ICR2 invalid in x2APIC) |
| X2APIC reserved (tail) | 0x840-0x8ff | deny-gp | deny-gp | Architectural: reserved tail of the block ("available for future Intel extensions"); the blanket row keeps registers added by future silicon closed by default and makes the class a gapless partition of 0x800–0x8FF. | Intel x2APIC spec 318148 + Intel SDM Vol.3A ch.11 (reserved x2APIC MSR space); INTEGRATION.md §7 (x2APIC MSR surface); scope-versioning fragment (default-deny) |

## Questions

- [question] Vocabulary clash to resolve at merge: the scope/enforcement fragment
  ("Enforcement carve-outs") anticipates `emulate-apic` rows in this sub-table enforced "by
  the APIC virtualization configuration itself (split irqchip / userspace timer
  emulation)", and INTEGRATION.md §7 names split irqchip as the default plan — but at
  v6.18.35 that configuration cannot satisfy §7: split irqchip keeps the LAPIC in the
  kernel, its x2APIC MSRs are unfilterable (api.rst silently ignores filters over
  0x800–0x8FF) and unforwardable to userspace, and its timer runs on host hrtimers
  (`apic_get_tmcct` reads `ktime_get()`). This fragment therefore takes `deny-gp` across
  the block, hides CPUID.1:ECX[21], and moves the deterministic per-register semantics to
  the userspace xAPIC MMIO sub-table (contract section 5). The master contract and the
  scope fragment's carve-out wording must be updated to "userspace LAPIC, x2APIC hidden" —
  confirm that update, or produce the proof §7 demands ("revisit only with proof") that an
  in-kernel LAPIC path can be V-time-driven.
- [question] If vmm-core ever wants real x2APIC (to retire the xAPIC MMIO page or for
  exit-cost reasons), the only route at the pinned tag is a carried kernel patch — a
  V-time-driven in-kernel LAPIC timer plus filterable/forwardable x2APIC MSR accesses —
  per INTEGRATION.md §6's deferred kernel-work item. Does the project accept that patch
  burden, or is xAPIC-only frozen for v1? Until answered, every row above stands as
  `deny-gp` (safe by default; loosening is a contract version bump).
