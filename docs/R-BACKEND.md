# Ruling R-Backend: decouple the trap apparatus behind a `Backend` seam

## The ruling

The **trap apparatus** — the thing that owns VT-x, runs the vCPU, and surfaces VM-exits — is
**decoupled from the deterministic VMM above it** behind a single `Backend` trait. The
deterministic logic (the CPU/MSR contract dispositions, V-time, hypercalls, snapshot/restore,
the userspace xAPIC/PIT models) sits entirely above that trait and **must not assume which
backend is in use**.

There are three interchangeable implementations, on a deliberate optionality ladder — **no
one-way doors**:

| Impl | Status | Role |
|---|---|---|
| `KvmBackend` (stock KVM, `kvm-ioctls`) | **bring-up default — *not* determinism-complete** | The known quantity; spikes 07/08 run on it. Deterministic for the surface it *can* trap (CPUID/MSR/port-IO/MMIO/xAPIC/hypercalls — the bulk), but it **cannot surface RDTSC/RDTSCP/RDRAND/RDSEED**, which are real determinism **holes** on this backend (KVM offsets the TSC in-kernel off host time; RDRAND hits the real RNG). For development + the deterministic subset only. |
| `PatchedKvmBackend` | **determinism baseline — RATIFIED (this ruling)** | The *first* determinism-**complete** backend and **the chosen one**: a small (~low-hundreds-of-lines) out-of-tree patch surfaces the exits stock KVM swallows (RDTSC/RDTSCP, RDRAND/RDSEED) to userspace, following the `KVM_X86_SET_MSR_FILTER` precedent. **This is the backend determinism is claimed on.** See "Implementation" below. |
| `DirectVmxBackend` | **preserved option — max isolation** | Own the VMCS via a custom kernel module. Built only if patched-KVM proves insufficient. See "What direct-VMX rebuilds" below. |

**The holes are explicit and fail-closed, never silent.** They are exactly the contract's §1
**backend-dependent** rows — a small, *enumerated* set (RDTSC/RDTSCP, RDRAND/RDSEED, and
`0x6e0` enforcement), not unknown gaps. `KvmBackend::run` cannot surface those exits, so it
must **refuse to claim determinism** for them rather than return a host-derived value as if it
were deterministic. The forcing function is the **determinism gate** (PLAN.md: same seed twice
⇒ identical state hash, run by the unison): the moment a guest reads RDTSC/RDRAND on stock
KVM, the gate **fails loudly**. So you cannot accidentally ship "determinism with holes" — the
real Linux payload (which reads the TSC constantly) simply will not pass the gate until it runs
on `PatchedKvmBackend` or `DirectVmxBackend`. Stock KVM is for getting *running*; determinism
is claimed one rung up.

This **resolves the CPU/MSR contract's §1 `[question]`-Backend**: the substrate is *decoupled*,
and **patched-KVM is the ratified determinism backend** (stock KVM for bring-up, direct-VMX a
preserved option) — with the contract's *dispositions* (the **what**) backend-agnostic and
the **how** living below the trait. The contract's "backend-dependent" rows (RDTSC, RDRAND,
RDSEED, `0x6e0` enforcement) are exactly the exits stock KVM can't surface — they name the
floor `KvmBackend` can't meet, which `PatchedKvmBackend`/`DirectVmxBackend` raise.

## Implementation: the patch, and the one deferred optimization

The patch is out-of-tree against the pinned KVM version, and small — ~low-hundreds of lines in
`arch/x86/kvm/vmx/vmx.c`, `arch/x86/kvm/x86.c`, `include/uapi/linux/kvm.h`, doing three
mechanical things: (1) enable the `RDTSC`/`RDTSCP`/`RDRAND`/`RDSEED`-exiting VMX controls KVM
leaves off; (2) add `KVM_EXIT_*` reasons for them; (3) plumb the exits to userspace — exactly
the shape of the existing `KVM_X86_SET_MSR_FILTER` (`KVM_EXIT_X86_RDMSR/WRMSR`) feature. The
ongoing cost is the rebase treadmill (re-apply per kernel version, in the hot exit path), not
the patch size. RDRAND/RDSEED are infrequent → the simple userspace round-trip is fine. `0x6e0`
needs no patch (the contract hides it).

**The one known perf risk — RDTSC — is deferred and data-gated (decision, this ruling).** RDTSC
is hot (timekeeping; the guest-userspace vDSO clock path if enabled), so a userspace exit per
RDTSC *may* dominate end-to-end overhead. We **ship the simple userspace-exit route and do not
pre-optimize.** Whether to move RDTSC's V-time computation in-kernel (faster, but couples KVM to
the `VClock` formula) is decided on **measurement, not speculation**:

- **The backend records per-exit-reason (per-trap-type) counts every run**, surfaced in the
  unison report — a cheap, normative observability requirement of `Backend`. This is *how*
  the optimization decision gets made: by data, during real runs.
- If those counts show **RDTSC traps dominating** measured overhead, *then* optimize — first try
  the cheaper guest-layer lever (route timekeeping through a hypercall / disable the vDSO TSC
  path — it's our image), and only if needed move RDTSC in-kernel. Until the numbers say so, the
  simple patch stands. **Logged here as an explicit area of future improvement.**

## The seam

The project already has the pieces — this promotes them into one boundary:
`vmcall-transport::VmExit` (guest-side abstraction of the `vmcall` instruction),
`vtime::CpuBackend`, and INTEGRATION.md §3's "vmm-core owns the `KVM_RUN` loop" /§5 inversion
seam.

```rust
/// The trap apparatus, decoupled from the deterministic VMM above it.
/// One impl per substrate; NOTHING above this trait may branch on which one.
pub trait Backend {
    /// Map a guest-physical region to host-owned, pinned backing store (pre-populated;
    /// no demand paging — a determinism choice, see below).
    fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()>;

    /// Run the vCPU until an exit needs the VMM. Blocking. The returned `Exit` is the
    /// ONLY channel by which the guest becomes observable.
    fn run(&mut self) -> Result<Exit>;

    /// Run until an exact V-time (retired-branch count) deadline, then exit — the §2
    /// "inversion" seam (the event loop drives the InjectionPlanner). PMU + single-step
    /// under the hood; on `DirectVmxBackend` the PMU freeze across VMRESUME/VMEXIT is
    /// owned directly (cleaner count-neutrality than via KVM).
    fn run_until(&mut self, deadline: VTime) -> Result<Exit>;

    /// Inject a vector at the next safe entry. The VMM decides WHEN (a V-time boundary);
    /// the backend writes the VM-entry interruption-info field.
    fn inject(&mut self, event: Event) -> Result<()>;

    /// Full guest-visible vCPU state for snapshot/restore: GPRs, segments, CR/XCR0,
    /// debug registers, and the `allow-stateful` MSR set (CPU-MSR-CONTRACT §3 / §4).
    fn save(&self) -> VcpuState;
    fn restore(&mut self, state: &VcpuState) -> Result<()>;
}

/// Every way the guest can become observable. **Default-deny is structural:** an op not
/// represented here either never exits (the backend never enabled its exit control) or is
/// a contract violation. The variants ARE the CPU/MSR contract's trapped surface.
pub enum Exit {
    Io      { port: u16, size: u8, write: Option<u32> },
    Mmio    { gpa: Gpa, size: u8, write: Option<u64> },   // the xAPIC page (R1)
    Hypercall { regs: HypercallRegs },                    // §1 VMCALL transport ABI
    Cpuid   { leaf: u32, subleaf: u32 },
    Rdmsr   { index: u32 },
    Wrmsr   { index: u32, value: u64 },
    Rdtsc,                                                // backend-dependent (contract §1)
    Rdrand  { width: u8 },                                // backend-dependent (contract §1)
    Shutdown,
    // …the closed set the contract enumerates; nothing else is reachable.
}
```

Normative rules:
- **Default-deny is enforced by the backend, structurally**: the VMM services only the `Exit`
  variants; an instruction whose exit control the backend didn't set never reaches the guest's
  observation, so the contract's "exhaustive default-deny" is the trait's natural posture.
- **Backend-dependent exits** (`Rdtsc`, `Rdrand`, `Wrmsr{0x6e0}`): `KvmBackend::run` cannot
  surface these (stock KVM handles/swallows them in-kernel) and must fail closed / refuse to
  claim determinism for them; `PatchedKvmBackend`/`DirectVmxBackend` surface them. This is the
  one place the contract's §1 backend dependency is observable, and it's localized here.
- **Nothing above the trait may branch on the impl.** The contract, V-time, hypercalls,
  snapshot, and device models compile against `Backend` alone.
- **Per-exit-reason trap counts are recorded every run** and surfaced in the unison report.
  Cheap, and it's the empirical input that gates the deferred RDTSC optimization (see
  Implementation) — the optimize/don't decision is made on these counts, not on speculation.

## What direct-VMX rebuilds (and what it doesn't) — the optionality cost

KVM is a known quantity; the decoupling means you **keep it** and only pay this cost if you
ever build `DirectVmxBackend`. The rebuild is bounded because the project's narrow surface
sidesteps KVM's hardest parts:

- **Rebuild (small, stable):** the VMX world-switch + run loop (VMXON/VMCS/VMRESUME +
  host/guest save-restore), a **deliberately simple EPT** (single-vCPU, fixed memory map,
  pre-populated, **no demand paging** — itself a determinism win), the MSR/CPUID/IO bitmaps,
  and event injection. Hooks into Linux as a loadable kernel module exposing `/dev/<vmm>`
  (ioctl + mmap) — structurally "your own minimal KVM"; the host runs normally (type-2).
- **Care needed:** NMI/MCE handling and world-switch hardening — the genuine risk area.
- **Skip entirely (KVM's scariest parts, unused here):** the general x86 instruction emulator
  (the default-deny surface needs only a small known decode), and the demand-paging MMU.
- **Already replaced regardless of backend:** the in-kernel irqchip and hrtimer/TSC-deadline
  timers — R1 moved these to the userspace xAPIC + V-time `TimerQueue`.
- **Never needed:** nested virt, live migration, IOMMU/device-assignment, dirty-ring, hotplug.

So what you'd "lose" from KVM is mostly functionality that is itself a *source* of the
nondeterminism the project fights; rebuilding it simpler is the win, not a regression. And you
never lose KVM as the safety net — it stays the default `Backend`.

## Follow-ups (not in this doc)

- Cross-reference from `docs/INTEGRATION.md` (it owns cross-component seams) and the CPU/MSR
  contract §1 `[question]`-Backend (point it at this ruling) — once PR #10 / PR #21 land, to
  avoid churn on their in-flight branches.
- A `ROADMAP.md` entry marking R-Backend resolved.
- A future `tasks/NN-backend.md` to spec the `Backend` crate + `KvmBackend` at vmm-core
  bring-up (vmm-core is still frontier work per INTEGRATION.md). The trait shape here is the
  starting contract; expect refinement when the real `KVM_RUN` loop is wired.
