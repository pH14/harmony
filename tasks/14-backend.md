# Task 14 — `consonance/vmm-backend`: the `Backend` trait + `KvmBackend` (R-Backend)

Read `tasks/00-CONVENTIONS.md` first. Touch only `consonance/vmm-backend/`.

## Environment

> *Requires (for the live `KvmBackend` path only): Linux bare-metal Intel x86-64 with VMX
> and `/dev/kvm`; does not run on macOS or under nested virtualization. The portable parts —
> the `Backend` trait and the `Exit`/`Event`/`VcpuState` value types — are Mac-unit-testable
> against a mock `Backend`; live `KVM_RUN` is box-only.*

This crate is **dual-natured** (per `docs/BRINGUP.md` §"Crate structure"):

- **Portable surface** (compiles and is fully tested on **macOS and Linux**): the `Backend`
  trait, the `Exit`/`Event`/`VcpuState`/`Capabilities`/`ExitCounts`/`BackendError` value
  types, and a deterministic in-process `MockBackend` for unit/property tests. No syscalls,
  no `/dev/kvm`, no ioctls.
- **Linux-only impl** (`KvmBackend`): the `kvm-*` dependencies and the `KvmBackend` type live
  under `[target.'cfg(target_os = "linux")'.dependencies]` + `#[cfg(target_os = "linux")]`,
  so `cargo build -p vmm-backend` stays green on a Mac (trait + types, no impl). Building and
  running `KvmBackend` live is **box-only**: the determinism box, reached as `ssh <det-box>`,
  CPU-pinned per `docs/BOX-PINNING.md`.

`std` crate (uses `Vec`/`BTreeMap`, `std::io::Error`). Not `#![no_std]`.

## Context

Ruling **R-Backend** (`docs/R-BACKEND.md`) decoupled the **trap apparatus** — the thing that
owns the vCPU and surfaces VM-exits — from the deterministic VMM above it, behind a single
`Backend` trait. The CPU/MSR-contract dispositions, V-time, hypercalls, snapshot/restore, and
the userspace xAPIC/PIT models all sit **above** that trait and **must not branch on which
backend is in use**. This crate is the trait + its first implementation. It is the lower half
of the `docs/BRINGUP.md` "Crate structure" split; `consonance/vmm-core` (task 15) is the upper
half and compiles **against this crate's `Backend` trait alone** — it is `KVM_RUN`-unaware
(`KVM_RUN` lives *inside* `KvmBackend::run()`, below the trait). **Task 14 leads task 15.**

Three interchangeable implementations are planned, on a deliberate optionality ladder
(R-Backend §"The ruling"); **this task delivers the trait + the first**:

| Impl | This task | Role |
|---|---|---|
| `KvmBackend` (stock KVM, `kvm-ioctls`) | **delivered here** (bring-up subset) | Bring-up default — **not** determinism-complete. Deterministic for the surface it *can* trap; declares (it does not launder) the RDTSC/RNG holes it cannot. |
| `PatchedKvmBackend` | not here (later; ratified determinism baseline) | Out-of-tree patch surfaces the exits stock KVM swallows. The backend determinism is *claimed* on. |
| `DirectVmxBackend` | not here (preserved option) | Own the VMCS via a custom module. Built only if patched-KVM proves insufficient. |

R-Backend's trait sketch and `Exit` enum are the **starting contract**; this task refines them
into a precise, implementable Public-API contract (signatures, error type, exit/completion
semantics, the closed variant set). R-Backend §"Follow-ups" anticipates this: *"The trait shape
here is the starting contract; expect refinement when the real `KVM_RUN` loop is wired."*
The refinements vs. the sketch are called out inline (search **`[refinement]`**) and summarized
in `IMPLEMENTATION.md` for review.

### The device model this backend runs under (R1)

`KvmBackend` creates the VM with **`KVM_IRQCHIP_NONE`** (`docs/R1-DEVICE-MODEL.md`): it calls
neither `KVM_CREATE_IRQCHIP` nor `KVM_CAP_SPLIT_IRQCHIP`. There is **no in-kernel
irqchip/LAPIC/PIT**; the guest LAPIC is a userspace **xAPIC** whose MMIO page (`0xFEE0_0000`)
falls through to `KVM_EXIT_MMIO` (→ `Exit::Mmio`). No x2APIC, no TSC-deadline timer. Interrupts
reach the guest **only** by the VMM calling `inject` at a V-time-chosen boundary (the
`KVM_INTERRUPT` queue). This is the only configuration in which the run loop's "nothing reaches
the guest without our injection" property holds (R1 §"What this buys").

## Portability & cfg gating (load-bearing — rule #6 nuance)

The standard convention is "no `#[cfg(target_os)]` logic forks." This crate is the
**deliberate, reviewed exception** sanctioned by `docs/BRINGUP.md` §"Crate structure": it is
frontier, box-only code, *not* a Mac-portable delegated worker crate. The gating is mechanical,
not a logic fork:

- The portable surface (trait + value types) has **no `cfg`** and is identical on every platform.
  `MockBackend` is also platform-portable (no `target_os` fork) but lives behind the non-default
  **`mock`** feature (§"Features"): absent from a default build, present under `--features mock` —
  **not** `#[cfg(test)]` (invisible to task 15 downstream) and **not** unconditional (it would ship
  in production). The "no `cfg`" rule above is about `target_os` logic forks, not the `mock` feature.
- `KvmBackend`, every `kvm-*`/`vm-memory`/raw-pointer item, and the live integration tests are
  `#[cfg(target_os = "linux")]`. `src/lib.rs` re-exports `KvmBackend` only under that cfg.
- A macOS `cargo build -p vmm-backend` compiles the trait + value types (+ `MockBackend` under
  `--features mock`) and **nothing else**; it must succeed with zero warnings.

## Dependencies — reviewed rule-5 whitelist exception (recorded here, NOT a `deny.toml` edit)

`kvm-ioctls`, `kvm-bindings`, and `vm-memory` are **not** on the `tasks/00-CONVENTIONS.md`
rule-5 whitelist (that whitelist governs Mac-portable worker crates). Per `docs/BRINGUP.md`
§"Crate structure" / §"Dependency note", **there is no `deny.toml` crate allowlist to add them
to** — `deny.toml` gates licenses/advisories/sources only and leaves the rule-5 *crate*
whitelist as a **review-time** gate. These three deps therefore enter as a **reviewed rule-5
whitelist exception, recorded in this task spec and the PR description** (rule 5's
ask-by-comment). **Do not edit `deny.toml`'s allow set** (there is none); `cargo deny check`
must still pass on the licenses/advisories it does gate.

- Linux-only (`[target.'cfg(target_os = "linux")'.dependencies]`): `kvm-ioctls`,
  `kvm-bindings`, `vm-memory`, and `libc` (`libc` is already whitelisted). Caret defaults, pin
  nothing.
- All-target: `thiserror` (whitelisted). `zerocopy` (whitelisted) is permitted for the
  `VcpuState` POD records if it helps determinism.
- **Features:** a non-default **`mock`** feature gates `MockBackend` (and any `proptest`/
  `Arbitrary` helpers it needs) so task 15 can turn it on under `[dev-dependencies]`. It must
  **not** be `#[cfg(test)]` — `#[cfg(test)]` items are invisible to downstream crates, so a
  test-only mock could not be the substrate task 15 unit-tests against. The default build
  compiles the trait + value types only; no other features.

**`unsafe` grant (rule #7).** This task **grants `unsafe`** for two named Linux-only purposes,
each `#[cfg(target_os = "linux")]` and each with a `// SAFETY:` comment: (1) registering the
host backing pointer with `KVM_SET_USER_MEMORY_REGION` in `map_memory`, and (2) `mmap`-ing the
`kvm_run` shared structure for the vCPU. Separately, the trait method `map_memory` is itself an
**`unsafe fn`** — a caller-precondition marker (the backend retains the host pointer past the
borrow; see its `# Safety` contract), not an `unsafe` operation. Declaring and implementing an
`unsafe fn` requires no `unsafe` *block*, so the portable surface (`MockBackend` included) still
contains **no `unsafe` blocks**: its `map_memory` body merely records the region.

## Determinism discipline (rule #4)

The backend is the floor the whole determinism edifice stands on; it must add no nondeterminism
of its own. Concretely: `VcpuState` and `ExitCounts` are **deterministic, canonical** — equal
guest state ⇒ equal `VcpuState` bytes/fields (the MSR set is a `BTreeMap<u32, u64>`, never a
`HashMap`; `zerocopy` records fully initialized incl. reserved/pad bytes). No floating point. No
wall-clock. `save()` **must never launder a host-derived value** (a host TSC, a host RNG draw)
into `VcpuState` as if it were deterministic guest state (see §"The non-determinism posture").

## Public API

```rust
//! crate-level doc: the trap apparatus, decoupled behind the `Backend` trait
//! (ruling R-Backend). One impl per substrate; nothing above this trait may
//! branch on which one. `KvmBackend` is the bring-up (stock-KVM) impl.

/// Guest-physical address. `[refinement]` of R-Backend's bare `Gpa`: a transparent
/// newtype so an address can't be confused with a host pointer or a length.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[repr(transparent)]
pub struct Gpa(pub u64);

/// A V-time deadline for `run_until`. `[refinement]` of R-Backend's bare `VTime`:
/// the unit is a **retired-conditional-branch work count** — the same axis
/// `vtime`'s `work` and task 07's PMU measure — **not** nanoseconds. (vmm-core
/// converts vns↔work via `vtime`; the backend counts hardware events.)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct Vtime(pub u64);

/// crate result alias.
pub type Result<T> = core::result::Result<T, BackendError>;

/// The trap apparatus, decoupled from the deterministic VMM above it.
/// **Object-safe / dyn-compatible** so the binary's composition root can hold a
/// `Box<dyn Backend>` and inject `KvmBackend` vs `PatchedKvmBackend` at `fn main`
/// (`docs/BRINGUP.md`: "the one place a concrete backend is named"). No generic
/// methods, no `Self`-by-value returns.
pub trait Backend {
    // --- configuration (installed once, before the first run) ----------------
    // The *data* comes from `docs/CPU-MSR-CONTRACT.md` via vmm-core (task 15);
    // the *mechanism* (KVM_SET_CPUID2 / KVM_X86_SET_MSR_FILTER) lives in the impl.
    // These keep vmm-core impl-agnostic: it pushes policy through the trait rather
    // than reaching for a KVM ioctl. `CpuidModel`/`MsrFilter` are portable POD config
    // types defined below (§"`CpuidModel` / `MsrFilter`"), co-designed with task 15.
    // **Fail-closed:** the backend tracks whether BOTH have been installed, and
    // `run`/`run_until` return `BackendError::NotConfigured` until they have — a guest
    // must never reach `KVM_RUN` on KVM's host-derived CPUID/MSR defaults (a determinism
    // leak). A call-order acceptance gate asserts this.

    /// Install the frozen guest-visible CPUID model (`KVM_SET_CPUID2` on KVM).
    /// MUST be called before the first `run`/`run_until`; otherwise the guest sees
    /// KVM's host-derived defaults (boot- and determinism-breaking).
    fn set_cpuid(&mut self, model: &CpuidModel) -> Result<()>;

    /// Install the default-deny MSR policy. On KVM this enables
    /// `KVM_CAP_X86_USER_SPACE_MSR` with the full mask
    /// (`FILTER | UNKNOWN | INVAL`, CPU-MSR-CONTRACT §1) **then**
    /// `KVM_X86_SET_MSR_FILTER`, so a denied/unknown/invalid MSR access surfaces as
    /// `Exit::Rdmsr`/`Exit::Wrmsr` (loud) instead of a silent in-kernel `#GP`.
    fn set_msr_filter(&mut self, filter: &MsrFilter) -> Result<()>;

    // --- memory ---------------------------------------------------------------

    /// Map a guest-physical region to host-owned, pinned, pre-populated backing
    /// store (no demand paging — a determinism choice). `gpa` and `host.len()` MUST
    /// be 4 KiB-aligned. Bring-up uses a single memslot; overlapping/duplicate maps
    /// error.
    ///
    /// **`unsafe`** because the backend registers `host`'s pointer with KVM and
    /// **retains it past this call** — the `&mut [u8]` borrow ends at return, but the
    /// guest writes through that pointer during every later `run`. The borrow checker
    /// cannot express "valid until the backend is dropped," so soundness is a caller
    /// obligation, not a borrow. (Alternative considered: have the backend *own* the
    /// backing; rejected for bring-up because vmm-core also needs `&[u8]`/`&mut [u8]`
    /// access to the same region — the loader writes the image, the M2 hash reads it —
    /// so a single owner plus an `unsafe` registration is simpler than a shared handle.)
    ///
    /// # Safety
    /// The caller MUST guarantee that `host`'s backing (a) stays live at a fixed
    /// address — pinned, never reallocated or moved — until the backend is dropped or
    /// the region is replaced; (b) is not aliased by any other live `&`/`&mut` while a
    /// `run`/`run_until` is in flight; and (c) starts at a **4 KiB-aligned host
    /// address** (`host.as_ptr() as usize % 4096 == 0`). `KVM_SET_USER_MEMORY_REGION`
    /// requires the *userspace address itself* to be page-aligned, which a plain
    /// `Vec<u8>`/slice does NOT guarantee (KVM rejects it with `EINVAL`) — back the
    /// region with an `mmap`/page-aligned allocation. Violating (a)/(b) is a
    /// use-after-free or data race; that unenforceable invariant is why this is `unsafe`.
    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()>;

    // --- run loop -------------------------------------------------------------

    /// Run the vCPU until an exit needs the VMM. Blocking. The returned `Exit` is
    /// the ONLY channel by which the guest becomes observable. Before resuming a
    /// read-style, `Wrmsr`, or `Hypercall` exit, the VMM MUST call the matching
    /// completion method (`complete_read` / `complete_ok` / `complete_fault` /
    /// `complete_hypercall` / `complete_cpuid`); calling `run` again with such an exit
    /// un-serviced is `BackendError::PendingCompletion` (fail closed). Increments the
    /// per-reason counter for the exit it returns. Returns `BackendError::NotConfigured`
    /// if called before `set_cpuid` AND `set_msr_filter` have both succeeded.
    fn run(&mut self) -> Result<Exit>;

    /// Run until an exact V-time (retired-branch) deadline, then exit with
    /// `Exit::Deadline` — the §2 inversion seam (PMU overflow-early + single-step
    /// under the hood; task 07 supplies the skid margin). A guest exit before the
    /// deadline returns that exit instead, short of `deadline`. **Bring-up
    /// `KvmBackend` returns `BackendError::Unsupported { what: "run_until" }`** —
    /// the live PMU/single-step path is Phase 2 (needs task 07 + the lapic
    /// injection seam); the trait declares it now so task 15 can compile against it.
    fn run_until(&mut self, deadline: Vtime) -> Result<Exit>;

    /// Inject a maskable IRQ (`KVM_INTERRUPT`) or NMI (`KVM_NMI`) at the next safe
    /// VM-entry. The VMM decides WHEN (a V-time boundary); the backend writes the
    /// entry-interruption field / queues the vector. **Bring-up `KvmBackend`
    /// returns `Unsupported { what: "inject" }`** (Phase 2; R1 §"injection
    /// plumbing"). The trait declares it now.
    fn inject(&mut self, event: Event) -> Result<()>;

    // --- exit completion (the read/write/hypercall round-trip) ----------------
    // `[refinement]`: R-Backend modeled reads as `write: Option<_>` but left the
    // return path implicit. KVM completes a read by writing into the shared
    // `kvm_run` buffer before the next entry; these methods make that explicit and
    // mockable. Exactly one completion call is valid per pending exit, before the
    // next `run`. `Io` OUT / `Hlt` / `Shutdown` need none. **`Wrmsr` is NOT a
    // no-completion exit**: a filtered `KVM_EXIT_X86_WRMSR` resumed *without* a
    // completion is taken by KVM as `msr.error == 0` (write succeeded), so a missed
    // handler would **silently allow** a write the contract may deny — `Wrmsr`
    // therefore stays pending until `complete_ok` (allow/drop) or `complete_fault`
    // (deny-gp), fail-closed like the read-style exits.

    /// Supply the value for a pending **read-style** exit: `Io { write: None }`,
    /// `Mmio { write: None }`, `Rdmsr`, or an instruction-read exit (`Rdtsc`,
    /// `Rdtscp`, `Rdrand`, `Rdseed`). The low `size`/`width` bytes are delivered to
    /// the guest's destination (for `Rdtsc`/`Rdtscp`, the full 64-bit TSC into
    /// `EDX:EAX`; `Rdtscp`'s `ECX = IA32_TSC_AUX` and `Rdrand`/`Rdseed`'s success
    /// `CF = 1` are fixed by the contract, not carried by this call). Errors
    /// `NoPendingRead` if no read-style exit is pending. (Stock `KvmBackend` never
    /// surfaces the instruction-read exits — §"non-determinism" — so it completes only
    /// IO/MMIO/MSR reads; the instruction-read completions exist for
    /// `PatchedKvmBackend`/`DirectVmxBackend`, declared now so task 15 compiles against
    /// the full set.)
    fn complete_read(&mut self, value: u64) -> Result<()>;

    /// The contract's `deny-gp` disposition for a pending `Rdmsr`/`Wrmsr`: inject
    /// `#GP` into the guest (on KVM, set `kvm_run.msr.error != 0`). Errors if the
    /// pending exit is not an MSR exit.
    fn complete_fault(&mut self) -> Result<()>;

    /// Resolve a pending `Wrmsr` whose contract disposition is **not** `deny-gp`:
    /// `allow` (the write is acknowledged) or `deny-ignore` (the write is dropped). On
    /// KVM both resume with `kvm_run.msr.error == 0`; the distinction (apply vs. drop)
    /// is the VMM's own bookkeeping — `EmulateVtime` writes, for instance, are applied
    /// to V-time state by vmm-core before this call. Required because a `Wrmsr` left
    /// pending fails closed (`PendingCompletion`) rather than silently allowing the
    /// write. Errors `BadCompletion` if the pending exit is not a `Wrmsr`.
    fn complete_ok(&mut self) -> Result<()>;

    /// Set guest `RAX` (the response-frame length per INTEGRATION.md §1, or 0 on
    /// transport error) for a pending `Hypercall`. Errors if none pending.
    fn complete_hypercall(&mut self, rax: u64) -> Result<()>;

    /// Supply the four result registers `(eax, ebx, ecx, edx)` for a pending
    /// `Exit::Cpuid` — `complete_read`'s single `u64` cannot carry four registers, which
    /// is why `Cpuid` has its own completion. Stock `KvmBackend` never surfaces `Cpuid`
    /// (it answers in-kernel from the `set_cpuid` table, so this is never called there);
    /// a backend that DOES surface it (`PatchedKvmBackend`/`DirectVmxBackend`) gets the
    /// quad from vmm-core, which builds it from `cpuid_model()` **and overlays the dynamic
    /// cells** (`CPUID.1:ECX[27]`←CR4.OSXSAVE, leaf `0xB`/`0x1F` subleaf echo,
    /// `0xD.0:EBX`←XCR0 — task 15's `resolve_cpuid`), so the frozen-table `CpuidModel` need
    /// not encode the dyn rows. Errors `BadCompletion` if the pending exit is not a `Cpuid`.
    fn complete_cpuid(&mut self, eax: u32, ebx: u32, ecx: u32, edx: u32) -> Result<()>;

    // --- snapshot / restore ---------------------------------------------------

    /// Full guest-visible vCPU state for snapshot/restore: GPRs/RIP/RFLAGS,
    /// segments + system registers, XCR0, debug registers, the contract's
    /// `allow-stateful` MSR set, the XSAVE image, pending-event/interrupt-shadow
    /// state, and the MP (run/halt) state. `[refinement]`: R-Backend's
    /// `fn save(&self) -> VcpuState` is fallible here — the underlying
    /// `KVM_GET_*` ioctls can fail and library code must not `unwrap` (rule #4).
    fn save(&self) -> Result<VcpuState>;

    /// Restore a `VcpuState` produced by `save`. Validates internal consistency;
    /// `InvalidState` on a malformed/incompatible blob (never a panic).
    fn restore(&mut self, state: &VcpuState) -> Result<()>;

    // --- observability (R-Backend normative) ----------------------------------

    /// Per-exit-reason trap counts since the last reset. **Recorded every run**
    /// (R-Backend §"Normative rules") and surfaced in the unison report; it is
    /// the empirical input that gates the deferred RDTSC optimization. Cheap, always
    /// on. Deterministic order.
    fn exit_counts(&self) -> ExitCounts;
    fn reset_exit_counts(&mut self);

    /// What determinism this backend can and cannot honestly provide. The unison
    /// report reads this to refuse to *claim* determinism for a payload that needs a
    /// capability the backend lacks (see §"The non-determinism posture").
    fn capabilities(&self) -> Capabilities;
}

/// Every way the guest can become observable. **Default-deny is structural:** an op
/// not represented here either never exits (the backend never enabled its exit
/// control / the instruction is serviced in-kernel) or is a contract violation that
/// fails closed as a `BackendError`. The variants ARE the CPU/MSR contract's trapped
/// surface; nothing else is reachable through the trait.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Exit {
    /// Port I/O. `write = Some(v)` = OUT(v); `write = None` = IN → `complete_read`.
    Io       { port: u16, size: u8, write: Option<u32> },
    /// MMIO (the xAPIC page, R1). `write = Some(v)` = store; `None` = load → read.
    Mmio     { gpa: Gpa, size: u8, write: Option<u64> },
    /// Filtered MSR read → `complete_read(value)` or `complete_fault()` (deny-gp).
    Rdmsr    { index: u32 },
    /// Filtered MSR write → `complete_ok()` (allow/drop) or `complete_fault()` (deny-gp);
    /// stays pending until one is called (no-completion would silently allow it).
    Wrmsr    { index: u32, value: u64 },
    /// VMCALL transport (INTEGRATION.md §1) → `complete_hypercall(rax)`. **Not
    /// surfaced by stock `KvmBackend`** — see the KVM-mapping note below.
    Hypercall(HypercallRegs),
    /// CPUID → `complete_cpuid(eax, ebx, ecx, edx)`. **Stock `KvmBackend` services CPUID
    /// in-kernel from the `set_cpuid` table and does not surface this**; a backend that
    /// does (`PatchedKvmBackend`/`DirectVmxBackend`) is completed with the dyn-overlaid
    /// quad (see `complete_cpuid`).
    Cpuid    { leaf: u32, subleaf: u32 },
    /// Backend-dependent (contract §1). **Not surfaced by stock `KvmBackend`** — a
    /// declared determinism hole, never a runtime trap. See §"non-determinism".
    Rdtsc,
    Rdtscp,
    Rdrand   { width: u8 },
    Rdseed   { width: u8 },
    /// `KVM_EXIT_HLT`. Idle-skip (INTEGRATION.md §3) or terminal; vmm-core decides.
    Hlt,
    /// `KVM_EXIT_SHUTDOWN` (triple fault / guest shutdown). Terminal.
    Shutdown,
    /// `run_until` reached the V-time deadline with no guest exit first. (Phase 2;
    /// stock `KvmBackend` never produces it in this task's scope.)
    Deadline { reached: Vtime },
}

/// An event the VMM injects. Aligned to R1's roster: maskable IRQs come only from
/// the `KVM_INTERRUPT` queue; NMI via `KVM_NMI`. (No other producer exists under
/// `KVM_IRQCHIP_NONE`.)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    Interrupt { vector: u8 },
    Nmi,
}

/// The VMCALL transport register frame (INTEGRATION.md §1): `RAX` = magic
/// `0x3150_4348`, `RBX` = request-page GPA, `RCX` = response-page GPA.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct HypercallRegs { pub rax: u64, pub rbx: u64, pub rcx: u64, pub rdx: u64 }

/// What this backend can honestly provide. Stock `KvmBackend` reports every
/// determinism field below `false`; `PatchedKvmBackend`/`DirectVmxBackend` raise them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities {
    /// Human-readable backend name for the unison report (e.g. "kvm-stock").
    pub name: &'static str,
    /// Surfaces RDTSC/RDTSCP as exits resolvable to a V-time value (NOT host TSC).
    pub deterministic_tsc: bool,
    /// Surfaces RDRAND/RDSEED as exits resolvable to a seeded stream (NOT host RNG).
    pub deterministic_rng: bool,
    /// Can loudly enforce a `deny-gp` on `IA32_TSC_DEADLINE` (0x6E0) writes.
    /// (Stock KVM swallows it in the WRMSR fastpath — R1 §"0x6E0"; moot under R1 as
    /// the guest never writes it, but declared honestly.)
    pub enforces_tsc_deadline_msr: bool,
}

/// Per-exit-reason trap counts (R-Backend observability). Plain `u64` counters with
/// a **deterministic** accessor order. Equal run ⇒ equal counts.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ExitCounts {
    pub io: u64, pub mmio: u64, pub rdmsr: u64, pub wrmsr: u64,
    pub hypercall: u64, pub cpuid: u64, pub rdtsc: u64, pub rdtscp: u64,
    pub rdrand: u64, pub rdseed: u64, pub hlt: u64, pub shutdown: u64,
    pub deadline: u64,
}
impl ExitCounts {
    /// Total trapped exits. Sum of all reasons.
    pub fn total(&self) -> u64;
    /// `(reason, count)` pairs in a fixed, deterministic order (for the report).
    pub fn entries(&self) -> [(ExitReason, u64); 13];
}

/// The discriminant of `Exit` (payload-free), for `ExitCounts::entries` and reports.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum ExitReason {
    Io, Mmio, Rdmsr, Wrmsr, Hypercall, Cpuid,
    Rdtsc, Rdtscp, Rdrand, Rdseed, Hlt, Shutdown, Deadline,
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    /// A trait method this backend does not implement (e.g. bring-up `run_until`).
    #[error("backend does not support: {what}")]
    Unsupported { what: &'static str },
    /// A required KVM capability is missing on this host.
    #[error("missing capability: {cap}")]
    Capability { cap: &'static str },
    /// `run`/`run_until` called before both `set_cpuid` and `set_msr_filter` succeeded —
    /// running on host-derived CPUID/MSR defaults would leak nondeterminism (fail closed).
    #[error("backend not configured: set_cpuid + set_msr_filter required before run")]
    NotConfigured,
    /// `map_memory` misuse: bad alignment, overlap, or zero length.
    #[error("memory mapping error: {0}")]
    Memory(&'static str),
    /// `run` called with an un-serviced read-style/hypercall exit pending.
    #[error("exit awaiting completion before resume")]
    PendingCompletion,
    /// A completion method called with no matching pending exit.
    #[error("no pending read/hypercall exit to complete")]
    NoPendingRead,
    /// A completion did not match the pending exit (e.g. `complete_fault` on `Io`).
    #[error("completion does not match the pending exit")]
    BadCompletion,
    /// `restore` given a malformed or incompatible `VcpuState`.
    #[error("invalid vcpu state for restore")]
    InvalidState,
    /// KVM reported `KVM_EXIT_INTERNAL_ERROR` / `KVM_EXIT_FAIL_ENTRY`, or an
    /// otherwise-unhandled exit reason — fail closed, never silently continue.
    #[error("backend internal error: {0}")]
    Internal(&'static str),
    /// Underlying ioctl/syscall failure (errno). Portable across platforms.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
```

### `CpuidModel` / `MsrFilter` (portable configuration types)

Portable POD (no `cfg`, no `unsafe`), defined here so task 15 compiles against an exact
contract and the impl translates them to KVM ioctls. vmm-core builds both from
`docs/CPU-MSR-CONTRACT.md`; the backend never invents the data, it only installs what it is
handed.

```rust
/// The frozen guest-visible CPUID table (→ `KVM_SET_CPUID2` on KVM). One entry per
/// `(leaf, subleaf)` the contract enumerates. Deterministic: equal model ⇒ equal
/// bytes, and the impl emits entries to KVM in this `Vec` order.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct CpuidModel { pub entries: Vec<CpuidEntry> }

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct CpuidEntry {
    pub leaf: u32,
    pub subleaf: u32,
    /// `subleaf` is significant for this leaf (→ `KVM_CPUID_FLAG_SIGNIFICANT_INDEX`).
    /// `false` for leaves whose result ignores `ECX`.
    pub subleaf_significant: bool,
    pub eax: u32, pub ebx: u32, pub ecx: u32, pub edx: u32,
}

/// The default-deny MSR policy (→ `KVM_X86_SET_MSR_FILTER`, installed *after* the
/// `KVM_CAP_X86_USER_SPACE_MSR` `FILTER|UNKNOWN|INVAL` mask). It names ONLY the MSRs
/// KVM may keep servicing **in-kernel** (the contract's "KVM virtualizes this
/// correctly" set, CPU-MSR-CONTRACT §1). Every MSR outside these ranges — and every
/// unknown/invalid MSR — traps to userspace as `Exit::Rdmsr`/`Exit::Wrmsr`, where
/// vmm-core applies the contract disposition (`deny-gp` → `complete_fault`;
/// `allow-fixed`/`emulate-vtime` → `complete_read`). The *disposition* lives in
/// vmm-core; this filter only decides in-kernel-vs-userspace.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MsrFilter { pub allow_inkernel: Vec<MsrRange> }

/// A half-open MSR-index range `[base, base + count)`. The ranges in an `MsrFilter`
/// are sorted and non-overlapping (deterministic; the impl folds them into KVM's
/// `kvm_msr_filter_range` bitmaps).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct MsrRange { pub base: u32, pub count: u32 }
```

### `VcpuState` (portable plain-data; mirrors task 09 by design, depends on it not at all)

`VcpuState` is the **live, in-memory** vCPU snapshot the backend produces/consumes. It is the
counterpart to task 09's serialized `vm_state` blob: vmm-core marshals a `VcpuState` into a
`vm_state::VmState` for the codec. Per rule #2 this crate **does not depend on `vm-state`**; the
field set deliberately parallels task 09's records and is kept consistent by review. Required
fields (provenance in parentheses):

```rust
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct VcpuState {
    pub regs:      VcpuRegs,                 // KVM_GET_REGS  — GPRs, RIP, RFLAGS
    pub sregs:     VcpuSregs,                // KVM_GET_SREGS2 — segments, CRs, EFER, apic_base
    pub xcr0:      u64,                       // KVM_GET_XCRS  — live XCR0 (image is in `xsave`)
    pub debugregs: DebugRegs,                // KVM_GET_DEBUGREGS — DR0..3, DR6, DR7
    pub events:    VcpuEvents,               // KVM_GET_VCPU_EVENTS — pending exc/NMI/SMI, shadow
    pub mp_state:  MpState,                  // KVM_GET_MP_STATE — Runnable vs Halted
    pub msrs:      std::collections::BTreeMap<u32, u64>, // allow-stateful set; sorted (rule #4)
    pub xsave:     Vec<u8>,                   // KVM_GET_XSAVE2 — FPU/XSAVE image
}
// VcpuRegs/VcpuSregs/Segment/DebugRegs/VcpuEvents/MpState are flat little-endian POD
// records with the same shape as task 09's. Define them locally (rule #2); document
// each field's KVM-ioctl provenance. `MpState` = { Runnable, Halted }.
```

### `KvmBackend` (Linux-only) — construction & the KVM exit mapping

`#[cfg(target_os = "linux")]`. Constructor (not on the trait):

```rust
#[cfg(target_os = "linux")]
impl KvmBackend {
    /// Open `/dev/kvm`, `KVM_CREATE_VM`, **decline the in-kernel irqchip**
    /// (`KVM_IRQCHIP_NONE` — create neither `KVM_CREATE_IRQCHIP` nor split-irqchip),
    /// `KVM_CREATE_VCPU` (one vCPU), and `mmap` the `kvm_run` page. Memory is mapped
    /// separately via the trait's `map_memory` (single memslot for bring-up). CPUID
    /// and the MSR filter are installed via the trait's `set_cpuid`/`set_msr_filter`
    /// before the first run.
    pub fn new() -> Result<KvmBackend>;
}
```

`run()` issues `KVM_RUN` and maps the raw `kvm_run.exit_reason` into `Exit`, then bumps the
counter. The honest mapping under stock KVM + `KVM_IRQCHIP_NONE` (state it in `IMPLEMENTATION.md`):

| Raw `kvm_run` exit | `Exit` | Notes |
|---|---|---|
| `KVM_EXIT_IO` | `Io` | `complete_read` for IN; OUT resumes |
| `KVM_EXIT_MMIO` | `Mmio` | xAPIC page falls through here (R1) |
| `KVM_EXIT_X86_RDMSR` | `Rdmsr` | requires the `set_msr_filter` userspace-MSR mask |
| `KVM_EXIT_X86_WRMSR` | `Wrmsr` | ditto |
| `KVM_EXIT_HLT` | `Hlt` | idle-skip / terminal (vmm-core) |
| `KVM_EXIT_SHUTDOWN` | `Shutdown` | triple fault / shutdown — terminal |
| `KVM_EXIT_INTERNAL_ERROR`, `KVM_EXIT_FAIL_ENTRY` | — | `BackendError::Internal` (fail closed) |
| `KVM_EXIT_IRQ_WINDOW_OPEN`, `KVM_EXIT_INTR`, `KVM_EXIT_DEBUG` | — | run-loop control; consumed internally by `inject`/`run_until` (Phase 2), **never** surfaced as an `Exit` |
| any other reason | — | `BackendError::Internal` — default-deny, never a silent continue |
| **CPUID** | (none) | serviced **in-kernel** from the `set_cpuid` table; stock KVM emits no CPUID exit. CPUID determinism comes from the installed table, not a run-loop handler. |
| **VMCALL** | (none) | stock KVM services VMCALL in-kernel (`kvm_emulate_hypercall`, `-ENOSYS` in RAX for unknown nr); a **general** userspace VMCALL exit is not available without a patch. So stock `KvmBackend` does **not** surface `Exit::Hypercall` in this task's scope (M1/M2 use no hypercalls). See "Open questions". |
| **RDTSC/RDTSCP/RDRAND/RDSEED** | (none) | execute in-guest, **no `KVM_EXIT`** — the declared holes (next section). |

`save()`/`restore()` go over `KVM_GET/SET_{REGS, SREGS2, XCRS, DEBUGREGS, VCPU_EVENTS,
MP_STATE, XSAVE2}` + `KVM_GET/SET_MSRS`. **The MSR index list these enumerate is the
`allow-stateful` set, which is exactly `MsrFilter::allow_inkernel`** (the bidirectional in-kernel
allow rows, §`CpuidModel`/`MsrFilter`): the backend **retains the filter from `set_msr_filter`** and
walks those ranges' indices, so it needs no separately-supplied list and `VcpuState.msrs`
round-trips precisely that set. (`allow-fixed`/`emulate-vtime` rows are deliberately NOT here —
read-only fixed values are not mutable guest state, and `emulate-vtime` TSC state lives in
vmm-core's V-time and is folded into the hash *above* the trait.) A `KVM_GET_MSRS` the host rejects
surfaces as `BackendError`, never a panic.

## The non-determinism posture (the corrected framing — read `docs/BRINGUP.md` step 2)

R-Backend's sketch said backend-dependent exits "fail closed." `docs/BRINGUP.md` **corrects**
this for *stock* KVM and that correction is normative here: **`KvmBackend` cannot intercept
RDTSC/RDTSCP/RDRAND/RDSEED at all.** They execute in-guest and return host-derived values with
**no `KVM_EXIT`** — the backend never sees them, so there is **nothing to "fail closed" on
per-instruction**. Do **not** describe `KvmBackend` as trapping or failing-closed on these
instructions. The honest, required posture is **non-determinism-claiming**:

1. `capabilities()` reports `deterministic_tsc = false`, `deterministic_rng = false`,
   `enforces_tsc_deadline_msr = false`. The unison report surfaces this; it is how the
   harness **refuses to claim determinism** for any payload that executes those instructions.
   `KvmBackend` determinism holds **only** for the audited RDTSC/RNG-free payload subset
   (M2's `hello`/`compute`).
2. `save()` **must never** launder a host TSC (or any host-derived value) into `VcpuState` as if
   it were deterministic guest state.
3. The forcing function is the **determinism gate** (same seed twice ⇒ identical state hash): if
   a value from one of these instructions reaches hashed memory/`VcpuState`, the two runs diverge
   and the gate fails loudly. Any payload that needs these instructions deterministic requires
   `PatchedKvmBackend` (not this task).

So "fail-closed" here is **structural and declarative** (the backend refuses to *claim*
determinism it cannot provide, and the gate catches leaks) — **not** a per-instruction runtime
trap. The one *enumerated, closed* set of backend-dependent dispositions is RDTSC/RDTSCP,
RDRAND/RDSEED, and `0x6E0` enforcement (contract §1) — known holes, not unknown gaps.

## Semantics that must hold

- **Configured-before-run (fail closed).** `run`/`run_until` return `NotConfigured` until both
  `set_cpuid` and `set_msr_filter` have succeeded — never run a guest on host-derived CPUID/MSR
  defaults. The Mac mock gate asserts the call-order error; the box gate asserts a configured run.
- **Run-loop contract.** `run` returns exactly one `Exit` and increments exactly one counter.
  A read-style (`Io` IN / `Mmio` load / `Rdmsr` / the instruction-reads `Rdtsc` / `Rdtscp` /
  `Rdrand` / `Rdseed`), a `Wrmsr`, a `Hypercall`, or a `Cpuid` exit must be completed by the matching
  method **before** the next `run`; a second `run` with one pending is `PendingCompletion` (fail closed).
  `Wrmsr` is included because resuming it without a completion is a silent in-kernel *allow* (KVM
  sets `msr.error == 0`) — it needs `complete_ok` (allow/drop) or `complete_fault` (deny-gp). Only
  truly-write-style exits (`Io` OUT / `Hlt` / `Shutdown`) need no completion. `complete_*` with
  no/mismatched pending exit errors (`NoPendingRead`/`BadCompletion`), never panics.
- **Default-deny is structural.** The backend services only the `Exit` variants; it enables exit
  controls/filters for exactly the contract's trapped surface. An unhandled or unknown raw exit
  reason is `BackendError::Internal` — never a silent resume.
- **`save`/`restore` round-trip.** `restore(&s)` followed by `save()` reproduces an
  observationally identical `VcpuState` (`==`). `save()` is deterministic: equal guest state ⇒
  equal `VcpuState` (sorted MSR map; no host-derived fields). This is the per-vCPU input to the
  M2 state hash (`docs/BRINGUP.md` step 6).
- **Counters.** Reset at run start by the caller (`reset_exit_counts`) or accumulated across a
  run; `exit_counts()` is a cheap snapshot in deterministic order; surfaced in the unison
  report. Recorded **every** run (R-Backend normative).
- **`map_memory` pinning.** Backing is pre-populated and pinned (no demand paging); alignment
  and non-overlap enforced; the caller's lifetime guarantee is documented at the call site.
- **No panics on untrusted input** (rule #4): every method returns `Result`; malformed
  completions, bad offsets, and incompatible `VcpuState` are errors, not panics.
- **Capabilities honesty.** `capabilities()` reflects the actual backend; stock `KvmBackend`
  never reports a determinism capability it cannot back (§"non-determinism").

## Acceptance gates

Beyond the standard gates (build/nextest/clippy `-D warnings`/fmt/deny), which must pass on
**both macOS and Linux** (`cargo build -p vmm-backend` on a Mac compiles the trait + value types
+ `MockBackend` only — zero warnings):

**Miri (required — this crate grants `unsafe`).** Per the `unsafe ⇒ Miri` rule
(`tasks/00-CONVENTIONS.md` / `AGENTS.md`, merged in quality-g/#24), `cargo +nightly miri test -p
vmm-backend` must run **clean**, and `vmm-backend` joins the `miri` job's crate set in
`.github/workflows/quality.yml` (and `scripts/install-quality-tools.sh`'s `MIRI_CRATES`). The granted
`unsafe` is the `#[cfg(target_os = "linux")]` KVM FFI — `KVM_SET_USER_MEMORY_REGION` registration and
the `kvm_run` `mmap`. Only the **ioctl/mmap syscalls themselves** are genuinely un-Miri-able (Miri
can't execute them); the **pointer/region bookkeeping** around them (slot tracking, the
alignment/overlap/bounds checks `map_memory` enforces, offset math into the mapped `kvm_run`) is
ordinary Rust that Miri **must** cover — excluding it wholesale would put the only pointer-unsafe
logic outside the UB gate, defeating the rule's intent. **Required (the `vmcall-transport` pattern):**
factor that bookkeeping into a `cfg`-agnostic, Miri-driveable seam (a helper exercised by a loopback/
fake that performs no syscall), and gate **only** the raw ioctl/mmap call with `#[cfg(not(miri))]`.
`cargo +nightly miri test -p vmm-backend` must run **clean** over the trait / `Exit` / `VcpuState` /
`MockBackend` **and** that pointer-handling seam, and `vmm-backend` joins the `miri` job's crate set in
`.github/workflows/quality.yml` (and `scripts/install-quality-tools.sh`'s `MIRI_CRATES`). Document the
seam + the single excluded syscall in `IMPLEMENTATION.md`.

### Mac-testable (the portable surface — required, no `/dev/kvm`)

1. **`MockBackend` exists and drives the run-loop contract.** A deterministic in-process
   `Backend` impl behind the non-default **`mock`** feature (NOT `#[cfg(test)]`, which task 15
   could not see — see §"Features") scripted with a queue of exits. Proves the trait is
   implementable without KVM and exercises: the
   run→exit→complete→run cycle; `PendingCompletion` on a missed completion;
   `NoPendingRead`/`BadCompletion` on mismatched completions; per-reason counter increments;
   `save`/`restore` round-trip; `capabilities()` plumbing. **It is the substrate task 15 unit-tests
   vmm-core against.**
2. **Run-loop / completion proptest (core gate).** Generate arbitrary sequences of scripted exits
   + completions; assert against a reference model that `exit_counts()` matches the reason
   histogram, completion discipline is enforced exactly (every read completed before resume), and
   no sequence panics. ≥ 256 cases.
3. **`VcpuState` round-trip proptest.** Arbitrary `VcpuState` (random GPRs/sregs/debugregs, an
   arbitrary `allow-stateful` MSR map, a random-length XSAVE image) through `MockBackend`
   `restore` → `save` ⇒ `==`; equal states ⇒ equal `VcpuState` (BTreeMap order, no float). ≥ 256
   cases.
4. **Object-safety / dyn-compatibility.** A test constructs `Box<dyn Backend>` and drives it;
   compilation is the assertion (the composition-root injection must work).
5. **Exhaustiveness.** A `match` over `Exit` with no wildcard compiles (the variant set is closed
   and the contract surface is complete); `ExitCounts::entries()` covers every `ExitReason`.

### Box-only (the live `KvmBackend` — `#[cfg(target_os = "linux")]`, run on the box)

A `#[cfg(target_os = "linux")]` integration test (`tests/kvm_*.rs`) marked **`#[ignore]`** so the
standard gates — which run `cargo test … --all-features` — **compile but do not run** it. (A Cargo
feature like `kvm-it` is the wrong gate here: `--all-features` flips it on, which would run the live
test — and trip its fail-fast — on a Mac/CI host. `#[ignore]` keeps it out of the default set
regardless of features.) It is executed explicitly on the determinism box, **CPU-pinned per
`docs/BOX-PINNING.md`** (a spare core, e.g. `ssh <det-box> 'taskset -c 1 cargo test -p vmm-backend --
--ignored --test-threads=1'`; record the core used). **Fail-fast, never skip**: on a host without
`/dev/kvm`/VMX/Intel, fail with a message saying what's missing and where to run it — never
silently pass.

6. **Bring-up smoke.** `KvmBackend::new` (IRQCHIP_NONE, one vCPU, single memslot). Load a tiny
   hand-assembled stub (à la task 08's stub) that does an `OUT` to a port then `HLT`; assert
   `run()` yields `Exit::Io` (complete it) then `Exit::Hlt`; assert `exit_counts()` (`io == 1`,
   `hlt == 1`).
7. **`save`/`restore` round-trip on real KVM.** Set GPRs via `restore`, `save`, mutate, restore,
   re-save ⇒ `==`.
8. **MSR filter loudness (confirmatory).** With `set_msr_filter` installed, the stub does an
   `RDMSR` of a denied index ⇒ `Exit::Rdmsr` ⇒ `complete_fault()` ⇒ the guest observes `#GP` (not
   a silent value). Confirms the `FILTER|UNKNOWN|INVAL` mask is set (CPU-MSR-CONTRACT §1).
9. **Capability honesty.** `capabilities()` reports `deterministic_tsc == false`,
   `deterministic_rng == false`, `enforces_tsc_deadline_msr == false`.

Property tests ≥ 256 cases; keep total Mac `cargo test` under ~3 minutes (the box integration
test is separate — `#[cfg(target_os = "linux")]` + `#[ignore]`, run with `-- --ignored` on the box).

## Sequencing

**Task 14 leads task 15.** `consonance/vmm-core` (task 15: Multiboot loader, entry state, UART/exit
shims, the `KVM_RUN`/event loop, M1/M2 gates) compiles **against this crate's `Backend` trait**
and is `KVM_RUN`-unaware. The composition root (the binary's `fn main`, task 15) is the one place
a concrete backend is named. `run_until`/`inject` are declared by the trait here but their live
`KvmBackend` implementation is **Phase 2** (PMU single-step + `KVM_INTERRUPT`/window handshake,
needs task 07 + the lapic seam) — they return `BackendError::Unsupported` in this task's scope.

## Open questions

- **VMCALL exit surfacing vs. `docs/CPU-MSR-CONTRACT.md` (integrator ruling — does not block this
  task).** This spec holds that **stock `KvmBackend` does not surface `Exit::Hypercall`**: stock
  KVM services `VMCALL` in-kernel via `kvm_emulate_hypercall`, returning `-ENOSYS` in `RAX` for the
  transport's unknown hypercall number (`RAX = 0x3150_4348`), and offers no *general* userspace
  VMCALL exit — `KVM_CAP_EXIT_HYPERCALL` routes only the specific `KVM_HC_MAP_GPA_RANGE`. The
  merged `docs/CPU-MSR-CONTRACT.md` §1 instead lists VMCALL among the **stock-serviceable**
  dispositions ("VMCALL via `KVM_EXIT_HYPERCALL`") and names only TSC, RNG, and `0x6E0` as
  not-stock-serviceable. Both cannot be literally true for this transport ABI. Likely
  reconciliation: VMCALL is **backend-dependent** (a userspace VMCALL exit needs
  `PatchedKvmBackend`/`DirectVmxBackend`, exactly like TSC/RNG), so the contract's "stock-
  serviceable" label on VMCALL is the imprecise side — but this touches a **merged contract doc**,
  so it is an **integrator ruling**, not a unilateral edit here. M1/M2 use no hypercalls
  (`docs/BRINGUP.md`), so `KvmBackend`'s scope is unaffected; `Exit::Hypercall` stays in the trait
  for the patched/direct backends. **Action pending:** confirm against the pinned `linux-6.18.35`
  `arch/x86/kvm/x86.c::kvm_emulate_hypercall`, then correct whichever document is wrong.

## Non-goals

`PatchedKvmBackend` / `DirectVmxBackend` (later; this task is the trait + stock `KvmBackend`); the
KVM RDTSC/RDRAND/RDSEED-exit patch (R-Backend §"Implementation"); the live PMU/single-step
`run_until` and `KVM_INTERRUPT` injection paths (Phase 2 — declared, not implemented here); the
Multiboot loader, entry-state setup, UART/isa-debug-exit device shims, and the event loop (task
15 / `vmm-core`); the CPUID/MSR-filter **data** (`docs/CPU-MSR-CONTRACT.md` — this crate installs
whatever model/filter it is handed); the LAPIC register model (`consonance/lapic`, task 13); the
`vm_state` serialization codec (`consonance/vm-state`, task 09 — this crate produces the live
`VcpuState`, not its bytes); multi-vCPU; AMD/ARM. Do not depend on any sibling crate (rule #2) —
`Gpa`/`Vtime`/`VcpuState`/`HypercallRegs` are defined locally and mirror, not import, the project's
shared shapes.
