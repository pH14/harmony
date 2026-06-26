# Ruling R1 — Device-emulation model

This is ruling **R1** from `docs/ROADMAP.md` ("the three rulings that unblock wave 2"). It
fixes the interrupt/timer device model `vmm-core` presents to the guest, and thereby
**concretizes `docs/INTEGRATION.md` §4's `vm_state` checklist into an exact field set** —
which is precisely what unblocks task 09. It is a design artifact with the same authority as
INTEGRATION.md and `docs/CPU-MSR-CONTRACT.md`: vmm-core implements it; it does not negotiate
with it.

R1 was settled **ahead of bring-up by reading current KVM source** (Linux **v6.18.35**, the
contract's pinned tag, re-verified byte-identical in **v7.1.1**, the current stable as of
2026-06-19), because the mechanism is *forced* by what KVM does and does not let userspace
intercept — not by a preference. The load-bearing lines are cited in **§Verification**.

## The ruling

`vmm-core` runs each VM with **no in-kernel interrupt controller** — `KVM_IRQCHIP_NONE`: it
calls neither `KVM_CREATE_IRQCHIP` nor enables `KVM_CAP_SPLIT_IRQCHIP`. Every interrupt/timer
device the guest sees is emulated in the VMM process. The roster:

| Device | Decision |
|---|---|
| **Local APIC** | **Userspace, xAPIC only** (MMIO at `0xFEE00000`). Timer driven by `vtime::TimerQueue`: a guest write to the initial-count register (`APIC_TMICT`, MMIO offset `0x380`) becomes a V-time deadline; current-count is computed from V-time on read. **No x2APIC. No TSC-deadline timer.** |
| **IOAPIC** | **Omitted.** It exists only to route external device IRQ lines to LAPICs; there are no real devices, so there are no lines. |
| **PIC (8259)** | Minimal deterministic userspace stub for early-boot probing, or absent if the task-04 kernel config avoids it. Port dispositions already in `docs/CPU-MSR-CONTRACT.md`. |
| **PIT (8254)** | Minimal deterministic stub; boot calibration is unnecessary because the contract freezes the TSC frequency (CPUID `0x15`/`0x16`). Ports already in the contract. |
| **HPET / RTC-CMOS / ACPI-PM / kvmclock** | Absent or hidden per the contract. |

Interrupts reach the guest **only** by the VMM calling `KVM_INTERRUPT` at a V-time-chosen,
single-step-exact instruction boundary (the `vtime` planner path of INTEGRATION.md §2).

## Why — the mechanism forces it

Three findings, each verified from source, collapse the "split-irqchip vs. fully-userspace"
question into "userspace, and xAPIC-only":

1. **An in-kernel LAPIC timer is host-time, with no escape.** KVM's in-kernel LAPIC timer has
   exactly two backends and both are host physical time: a `CLOCK_MONOTONIC` hrtimer, or
   (Intel-preferred) the VMX preemption timer programmed from `rdtsc()`. TSC-deadline mode
   uses the same hrtimer. There is no module param, CAP, or ioctl to retarget it to a counter
   / virtual-time source. So **any** configuration that keeps the LAPIC in the kernel (full or
   split irqchip) cannot produce a V-time timer.

2. **You cannot keep the in-kernel LAPIC and trap only its timer.** The two ways to intercept
   LAPIC timer programming to userspace are both closed while the in-kernel LAPIC is live: the
   x2APIC MSR range `0x800–0x8FF` is **unfilterable** (`kvm_msr_allowed` returns `true` for it
   unconditionally, before any filter check), and `IA32_TSC_DEADLINE` writes are taken by an
   in-kernel **WRMSR fastpath** that is not filter-aware. `Documentation/virt/kvm/api.rst`
   states it outright: *"KVM does not support emulating x2APIC in userspace."* So the only
   place the VMM can see every LAPIC access is with the LAPIC **out** of the kernel.

3. **Userspace irqchip is first-class, and xAPIC traps cleanly — but x2APIC is impossible
   there.** Running with `KVM_IRQCHIP_NONE` is a maintained configuration (KVM keeps a
   dedicated `kvm_has_noapic_vcpu` static-key fast path; nothing is deprecated). With no
   in-kernel LAPIC the xAPIC MMIO page is not special-cased, so guest accesses fall through to
   `KVM_EXIT_MMIO` for the VMM to service. But x2APIC is hard-tied to the in-kernel LAPIC: with
   no in-kernel apic, an x2APIC register access `#GP`s. **And KVM will not stop us from
   advertising x2APIC in CPUID** — if we do, a modern guest enables it and then `#GP`s/panics
   on first use. Therefore the contract **must mask `X86_FEATURE_X2APIC`** (and the
   TSC-deadline bit, which KVM also won't back without an in-kernel apic); the guest uses the
   classic xAPIC timer via the initial-count MMIO register.

## What this buys: a near-proof of controlled interrupt delivery

The device model isn't merely compatible with determinism — in this exact configuration
(single-vCPU, `KVM_IRQCHIP_NONE`, APICv structurally off) KVM source shows **no interrupt or
event can reach the guest without the VMM's explicit injection**, which is ReVirt's
"asynchronous events only at reproducible boundaries" half made structural:

- **Maskable IRQs** are sourced *only* from the VMM's `KVM_INTERRUPT` queue (the extint path
  returns exactly `vcpu->arch.interrupt.nr`); there is no other producer.
- **APICv / posted-interrupts / virtual-interrupt-delivery / IPIv** are all gated on
  `lapic_in_kernel()` → structurally inactive; nothing delivers a vector behind our back.
- **NMI/SMI** arrive only via `KVM_NMI`/`KVM_SMI` / `KVM_SET_VCPU_EVENTS` (ours to issue).
- **Guest vPMU PMI** cannot be delivered without an in-kernel LAPIC (`kvm_pmu_deliver_pmi`
  no-ops).
- **Async page-fault** injection requires an in-kernel LAPIC (`kvm_can_do_async_pf` gate) →
  off. (Defense-in-depth still applies; see Constraints.)
- **In-kernel PIT/PIC** are not creatable without the in-kernel irqchip; **Xen** events are
  off unless explicitly enabled; host physical IRQs cause host-side VM-exits that never touch
  guest state.

Injection plumbing the run loop relies on (all confirmed present and stable in v6.18.35 and
v7.1.1): `KVM_INTERRUPT` + `kvm_run.request_interrupt_window` +
`kvm_run.ready_for_interrupt_injection` (ready = `RFLAGS.IF` set and no STI/MOV-SS shadow) +
`KVM_EXIT_IRQ_WINDOW_OPEN`; and `KVM_SET_GUEST_DEBUG`/`KVM_GUESTDBG_SINGLESTEP` for exact
landing, with **`KVM_GUESTDBG_BLOCKIRQ`** available to hard-suppress injection during stepping
runs.

## Consequence 1 — task 09 (`vm_state`) is now specifiable

The device portion of the snapshot blob is now a concrete, fully VMM-owned field set with **no
coupling** to KVM's `kvm_lapic_state` / `kvm_irqchip` / `kvm_pit_state2` ABI. The complete
`vm_state` blob:

- **Our device structs:** the `lapic` register file + timer state (below); PIC stub state;
  PIT stub state.
- **KVM vCPU sub-states** (captured via ioctl, serialized as our versioned records):
  `KVM_GET_REGS` (GPRs/RFLAGS/RIP), `KVM_GET_SREGS`/`SREGS2` (segments, CRs, `IA32_APIC_BASE`),
  `KVM_GET_XSAVE2` (FPU/XSAVE state image per contract §2 XCR0 policy), **`KVM_GET_XCRS`** (the
  live `XCR0`/XCR register file — `XSAVE2` carries the state *image*, not `XCR0` itself, and the
  guest may `XSETBV` within the §2-masked menu, so it must be captured or restore diverges),
  `KVM_GET_MSRS` over the contract's `allow-stateful` set, **`KVM_GET_DEBUGREGS`** (DR0–3/DR6/DR7
  — the guest can set hardware breakpoints; not covered by `GET_REGS`/`GET_SREGS`),
  `KVM_GET_VCPU_EVENTS` (pending exception/NMI/SMI, interrupt shadow), **`KVM_GET_MP_STATE`**
  (runnable vs `KVM_MP_STATE_HALTED` — a snapshot taken at an idle/`HLT` quiescent point, the
  common case per INTEGRATION.md §3, must record the halt or restore wrongly resumes a runnable
  vCPU and diverges when the next timer/IRQ is expected to wake it).
- **V-time:** `VClock::snapshot_vns(work)` + ratio config (integer ratio per INTEGRATION.md §4).
- **Timers:** `TimerQueue` contents (absolute V-time deadlines).
- **Hypercall:** `Dispatcher::save_state()` (task 01) — incl. the entropy PRNG position.
- **Invariant:** snapshot only at a quiescent point — no armed-but-unfired injection plan
  (assert, don't serialize), per §4.

Task 09 serializes that set with a versioned, round-trip-/proptest-tested codec. R1 is its
prerequisite; it can be specced now.

## Consequence 2 — a new delegable crate: `lapic`

"We emulate the LAPIC" is a self-contained, pure-logic unit, not frontier glue. Spin it out as
a crate in the mold of `vtime`/`snapshot-store`:

- `#![no_std]`, no `/dev/kvm`, no host deps; V-time in, deadlines + vectors out.
- Models the **xAPIC** register file (ID/TPR/PPR, IRR/ISR/TMR, LVT incl. LVT-timer, ICR for
  self-IPI, divide-config, initial/current count) and prioritized delivery (highest pending
  vs. TPR/PPR), EOI, and the timer → `TimerQueue` handoff.
- Snapshot/restore is its own struct (feeds task 09 directly).
- Gate-testable on a laptop with proptest (register/timer state machine) and Kani
  (priority/timer arithmetic) — the project's house style.

The KVM-facing adapter (routing `KVM_EXIT_MMIO` on the APIC page into the crate; calling
`KVM_INTERRUPT`; the interrupt-window handshake) stays frontier in `vmm-core`. R1 thus
*shrinks* the frontier: the LAPIC's logic leaves it.

## Constraints `vmm-core` and the contract must enforce

Defense-in-depth, several already in the contract:

1. **Mask `X86_FEATURE_X2APIC` in CPUID** (KVM won't, and an advertised-but-unbacked x2APIC
   panics the guest).
2. **Mask the TSC-deadline CPUID bit** (CPUID.1:ECX[24]); drive the timer via the xAPIC
   initial-count MMIO register.
3. **Keep all KVM PV CPUID leaves (`0x4000_00xx`) hidden** (already in the contract) — closes
   kvmclock and async-PF at the source.
4. **Do not advertise `KVM_FEATURE_ASYNC_PF`; `mlock`/pin guest memory** — async-PF is
   structurally off here, but pinning removes the last host-memory-management interaction with
   guest pages. (Determinism survives a swap regardless, since the clock is V-time, not
   wall-clock; pinning is for predictability and clean CoW.)
5. **Disable the guest vPMU** (clear PMU CPUID; `RDPMC` traps) — the host owns the PMU for
   V-time.
6. **Never** create an in-kernel irqchip/PIT or enable Xen; issue NMI/SMI only deliberately.
7. **Build KVM with `CONFIG_KVM_IOAPIC=n`** where available (Linux 6.17+) — compiles the
   in-kernel IOAPIC/PIC/PIT out entirely, build-enforcing the roster above and removing that
   emulation surface from the shipped kernel (stronger than declining the ioctl at runtime).

## Required follow-ups (not in this PR)

- **`docs/CPU-MSR-CONTRACT.md` (task 06, PR #10) — RESOLVED in the merged contract.** This
  note's premise was only half-right: the contract never dispositioned the x2APIC range as
  `emulate-apic` — it already **hid x2APIC** (`CPUID.1:ECX[21]=0`) and **denied `0x800–0x8FF`**
  (`deny-gp`), i.e. it was already R1-consistent (the "emulate-apic" claim was inaccurate). The
  real conflict was **`IA32_TSC_DEADLINE`**, and it has been redispositioned: the merged contract
  **hides the TSC-deadline bit** (`CPUID.1:ECX[24]=0`) and makes **`0x6E0` `deny-gp`** (the LAPIC
  timer is the xAPIC LVT one-shot/periodic via MMIO, §5), aligning fully with R1. No further
  contract change is forced by R1.
- **Task 09 spec** — authored (PR `docs/task-spec-09`, pending merge): `tasks/09-vm-state.md`
  (Consequence 1). The device-emulation portion of the blob is carried as an opaque
  length-delimited placeholder pending task 13, so the rest of the `vm_state` container/version is
  locked without waiting on `LapicState`. (Not yet on `main` — don't treat the container/version as
  locked until it merges.)
- **`lapic` crate spec** (roadmap task 13) — authored (PR `docs/task-spec-13`, this PR):
  `tasks/13-lapic.md` (Consequence 2). It owns `LapicState`; task 09 folds that typed struct into
  its device section as a follow-up (under a bumped `VM_STATE_VERSION`), which is why 13 leads 09.

## `IA32_TSC_DEADLINE` (`0x6E0`) enforcement — resolved by source

Whether a guest `IA32_TSC_DEADLINE` (`0x6E0`) write in userspace-irqchip mode is *filterable*
to userspace or *silently swallowed* by the in-kernel WRMSR fastpath was settled during the
CPU/MSR contract review **by reading v6.18.35 source**: it is **swallowed** — `handle_fastpath_
wrmsr` services `0x6E0` before the MSR filter and `kvm_set_lapic_tscdeadline_msr` no-ops with no
in-kernel apic. So `deny-gp` on `0x6E0` is **not enforceable on stock KVM** for an off-contract
writer (no logged `#GP`); it is therefore a **backend-dependent** disposition (patched-KVM /
direct-VMX), exactly as the merged contract (`docs/CPU-MSR-CONTRACT.md` §1, `docs/R-BACKEND.md`)
records. **For R1 itself this is moot:** the TSC-deadline CPUID bit is hidden and the timer is
driven via xAPIC MMIO, so the cooperative guest never writes `0x6E0`. The Phase-0 box test
(set the filter, have the stub write `0x6E0`, observe swallow) would be *confirmatory only*.

## The 2025–2026 KVM feature sweep (does anything new change R1?)

A two-pass survey of KVM changes from Linux v6.13 → v7.1 found **nothing that alters this
ruling**, two changes that *reinforce* it, and several adjacent findings routed below to the
tasks they touch (recorded so they aren't re-derived):

- **Reinforces R1 (×2):** v7.0 added **userspace control of EOI-broadcast suppression**
  (directly supportive of the userspace-xAPIC model), and **`CONFIG_KVM_IOAPIC`** (6.17) lets
  us compile the in-kernel IOAPIC/PIC/PIT out entirely (see Constraints §7) — the kernel can
  be built to support *only* our model. (KernelNewbies *Linux 7.0* / *6.17*.)
- **In-kernel LAPIC timer:** confirmed still un-retargetable from host wall-clock — no new
  knob. R1's premise holds at v7.1.
- **Adjacent → task 07 (PMU spike):** **mediated/passthrough vPMU** landed in **v7.0**
  (Christopherson series; LWN 959653) — *not* in v6.18. It gives a *guest* exclusive PMU
  ownership and adds a perf-core arbitration keyed on `exclude_guest`; our V-time counter is a
  `!exclude_guest` host event. Three reasons it doesn't threaten us: it is **default-off**
  (`enable_mediated_pmu` opt-in); it auto-enables **only for VMs with an in-kernel LAPIC** —
  which R1 forbids, so **our VM is structurally outside its scope and cannot be silently
  converted**; and even the worst case (a co-tenant mediated guest) fails **cleanly and
  detectably** — our counter goes to error state, never a silent miscount. Task 07 action on
  7.x: assert `enable_mediated_pmu=off`, keep mediated tenants off our cores (one-VM-per-core
  makes this enforceable), and verify an `exclude_host` counter still counts guest branches
  identically. Also keep the counter **exits-excluded** (`exclude_host`/`exclude_hv`) — rr
  documents that VM-exit clustering perturbs a retired-branch count otherwise.
- **Adjacent → task 08 (restore spike):** `guest_memfd` `mmap()` for ordinary VMs landed in
  **v6.18** (NUMA mempolicy in 6.19, userfaultfd *minor*-fault in 7.1). Useful as a substrate,
  **but a `guest_memfd` is per-VM and cannot be shared across VMs** — so the "one read-only
  post-boot image shared across many VMs" goal stays on the classic shared-file +
  `mmap(MAP_PRIVATE)` + remap model (untouched by gmem). Fast remap-restore remains userspace
  work; the 7.1 UFFD minor-fault hook is the one new primitive worth evaluating *if* task 08
  adopts gmem. Dirty-ring (`KVM_CAP_DIRTY_LOG_RING_ACQ_REL` + `KVM_RESET_DIRTY_RINGS`) and
  per-VM TSC control (`KVM_CAP_VM_TSC_CONTROL`) already exist on v6.18.
- **Adjacent → vmm-core / task 07 (injection):** a **single-step / `#DB`-during-emulation**
  hardening series (~v6.19) fixes single-step `#DB` injection that mishandled the interrupt
  shadow (MOV SS / VM-entry failures) — directly on the exact-count injection path. **Not in
  v6.18**: run the box on **6.19+** or backport.
- **Adjacent → `docs/ARM-PORT.md` (follow-up):** rr documents that **ARM LL/SC atomics make
  retired-instruction/branch counts non-deterministic** — a concrete, citable hazard for the
  AArch64 branch-counting bet, reinforcing that doc's existing "unproven on ARM" verdict.

## Verification (load-bearing citations)

Source read raw from the stable tree at **v6.18.35** (the contract's pinned tag) and
re-checked **byte-identical at v7.1.1** (current stable, 2026-06-19). Line numbers are
approximate (v6.18.35); the named functions are the stable anchors.

| Claim | Source |
|---|---|
| LAPIC timer is a `CLOCK_MONOTONIC` hrtimer | `arch/x86/kvm/lapic.c` `kvm_create_lapic()` — `hrtimer_setup(…, CLOCK_MONOTONIC, HRTIMER_MODE_ABS_HARD)` (≈3091) |
| TSC-deadline mode uses the same hrtimer | `lapic.c` `start_sw_tscdeadline()` — `hrtimer_start(…)` (≈2102) |
| Intel HV timer = host TSC | `arch/x86/kvm/vmx/vmx.c` `vmx_update_hv_timer()` — `tscl = rdtsc(); … VMX_PREEMPTION_TIMER_VALUE` (≈7223–7247) |
| x2APIC MSRs unfilterable | `arch/x86/kvm/x86.c` `kvm_msr_allowed()` — `if (index >= 0x800 && index <= 0x8ff) return true;` (≈1811) |
| KVM can't emulate x2APIC in userspace | `Documentation/virt/kvm/api.rst` (≈1849–1852); filter-scope note (≈4306–4307) |
| `TSC_DEADLINE` taken by in-kernel WRMSR fastpath | `x86.c` `handle_fastpath_set_msr_irqoff()` — `case MSR_IA32_TSC_DEADLINE` (≈2290) |
| `KVM_IRQCHIP_NONE` is first-class | `arch/x86/kvm/irq.h` `irqchip_in_kernel()`; `lapic.c` `kvm_create_lapic()` no-irqchip branch (≈3067–3070); `lapic.h` `kvm_has_noapic_vcpu` static key |
| xAPIC MMIO → `KVM_EXIT_MMIO` when no in-kernel apic | `x86.c` `vcpu_mmio_write()` — `lapic_in_kernel()` gate (≈7798–7817) |
| Maskable IRQ sourced only from `KVM_INTERRUPT` | `arch/x86/kvm/irq.c` `kvm_cpu_has_extint`/`kvm_cpu_get_extint` (≈72–73, 135–136); `x86.c` `kvm_vcpu_ioctl_interrupt()` no-irqchip queue (≈5402–5406) |
| "ready" = IF + no STI/MOV-SS shadow | `vmx/vmx.c` `__vmx_interrupt_blocked()` (≈5033–5037) |
| `KVM_EXIT_IRQ_WINDOW_OPEN` path | `x86.c` `vcpu_run()` (≈11698–11703) |
| APICv requires in-kernel LAPIC | `lapic.h` `kvm_vcpu_apicv_active()` — `return lapic_in_kernel(vcpu) && …` (≈211–214) |
| Guest PMI no-op without in-kernel LAPIC | `arch/x86/kvm/pmu.c` `kvm_pmu_deliver_pmi()` (≈705–708) |
| Async-PF requires in-kernel LAPIC | `x86.c` `kvm_can_do_async_pf()` — `lapic_in_kernel` gate (≈13906) |
| `KVM_GUESTDBG_BLOCKIRQ` suppresses injection | `x86.c` `kvm_check_and_inject_events()` (≈10778–10779); `api.rst` (≈3755) |

External references for the feature sweep: mediated vPMU — LWN 959653, `lkml.org/lkml/2026/2/6/1887`;
`guest_memfd` mmap — LWN 1031923; `CONFIG_KVM_IOAPIC` — LWN 1025123; v7.0 summary —
`kernelnewbies.org/Linux_7.0`; rr retired-conditional-branch technique & hazards —
`robert.ocallahan.org/2020/12/exploiting-precognition-in-binary.html`.
