// SPDX-License-Identifier: AGPL-3.0-or-later
//! The engine/vendor seam (`docs/ARCH-BOUNDARY.md` §B): everything in this crate
//! **outside** this module is the arch-neutral **engine** — the run-loop
//! skeleton, guest RAM, the snapshot engine, the state-hash *framework*
//! (canonical chunk list → hash), the control server, the corpus adapter, the
//! work seam, and the V-time/idle wiring — and speaks only
//! `(Gpa, Moment, bytes, hashes)` plus the common exit vocabulary. Everything
//! **inside** a vendor submodule is that architecture's own: the CPU contract
//! and its installed policy, the exit dispatch and dispositions, the boot
//! loaders and entry state, the interrupt fabric and platform device models, the
//! host-homogeneity probe, the work-counter event, and the state records.
//!
//! [`Vendor`] is how the engine reaches the vendor half without naming it: the
//! engine's [`Vmm`] holds `<B::A as Vendor>::Devices` and dispatches arch exits
//! through [`Vendor::dispatch_arch`], so it can neither match a vendor's exit
//! enum nor touch a vendor's devices — arch-blindness is compiler-checked, and
//! each vendor's dispatch matches its own exit enum exhaustively (no wildcard
//! arms; default-deny stays structural).
//!
//! **Module split, not crate split**: the reserved engine/vendor *crate* names
//! activate with the ARM window (`docs/GLOSSARY.md` "Reserved — consonance");
//! until then the boundary is this trait and these module lines.
//!
//! Like [`Backend`], this trait is **designed, not frozen** — the AA-3
//! trait-freeze memo (the ARM spike) owns the freeze decision.

use control_proto::RegsView;
use vm_state::VmState;
use vmm_backend::{Arch, Backend, Gpa};

use crate::vmm::{Step, Vmm, VmmError};

pub mod x86;

/// Why a vendor refuses a wire-format interrupt identity at **stage time** — a
/// recoverable rejection the control plane turns into a reply, rather than letting
/// it explode later as a session-fatal apply-time error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InterruptReject {
    /// This machine has no interrupt fabric wired to deliver into — a permanent
    /// limitation of its composition (x86: no userspace LAPIC).
    NoFabric,
    /// The identity lies outside this vendor's identity space entirely (x86: past
    /// the xAPIC's 8 bits). The wire field is a `u32` precisely because identities
    /// are per-arch and a GIC INTID exceeds 8 bits (`docs/ARCH-BOUNDARY.md` §C).
    OutOfRange,
    /// The identity is architecturally **reserved** on this vendor and cannot be
    /// raised (x86: vectors `< 16`) — a request error the client can fix. The
    /// **vendor** narrows it to the wire error's width, which it can do safely
    /// because it knows its own reserved range; the engine assumes nothing.
    Reserved {
        /// The reserved identity, as the control wire carries it.
        vector: u8,
    },
}

/// One vendor's half of the deterministic VMM. Implemented on the vendor's
/// [`Arch`] zero-sized type; the hooks take the engine's [`Vmm`] so the vendor
/// half reads and writes engine state through its `pub(crate)` surface (one
/// crate, one boundary — the trait *is* the line).
pub trait Vendor: Arch + Sized {
    /// The per-VM vendor device state: the interrupt fabric, the platform shims,
    /// and the serial device (x86: xAPIC + 8259/PCI latches + 8250).
    type Devices;

    /// The vendor half of a validated-but-uncommitted `vm_state` restore
    /// ([`validate_restore`](Vendor::validate_restore) →
    /// [`commit_restore`](Vendor::commit_restore)).
    type RestorePrep;

    /// Fresh (reset) device state for a new VM. Which fabric pieces are wired is
    /// the vendor composition root's job (e.g. `wire_lapic` on x86).
    fn new_devices() -> Self::Devices;

    /// The arch's **device-MMIO holes** as `(base, len)` — GPA ranges that are
    /// *not* guest RAM even when the RAM image spans them, because the backend
    /// deliberately leaves them out of its memslots (x86: the 4 KiB xAPIC page at
    /// `0xFEE00000`, which `KvmBackend::map_memory` splits around so guest
    /// accesses fault out to the device model instead of hitting RAM).
    ///
    /// The engine needs this to validate **guest-published GPAs**: a page the
    /// guest hands the host (the task-110 pvclock page) must be real, host-
    /// writable RAM. Inside a hole, the host would stamp backing the guest cannot
    /// see while the guest's own reads went to a device — a silently-wrong clock
    /// (cross-model r5 P2). Naming which addresses those are is vendor knowledge,
    /// so it lives behind this seam rather than in the engine
    /// (`docs/ARCH-BOUNDARY.md`).
    fn mmio_holes() -> &'static [(u64, u64)];

    // --- run-loop dispatch ---------------------------------------------------

    /// Dispatch one vendor exit against the contract dispositions and the device
    /// models. Matches the vendor's exit enum **exhaustively**.
    fn dispatch_arch<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        exit: Self::Exit,
    ) -> Result<Step, VmmError>;

    /// Route a [`CommonExit::Mmio`](vmm_backend::CommonExit::Mmio) access — which
    /// physical addresses hold device models is vendor knowledge (x86: the xAPIC
    /// page). An unmodeled address fails closed.
    fn dispatch_mmio<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        gpa: Gpa,
        size: u8,
        write: Option<u64>,
    ) -> Result<Step, VmmError>;

    // --- interrupt fabric ----------------------------------------------------

    /// Advance the fabric to the current V-time and hand the backend the one
    /// arbitrated deliverable interrupt identity (or `None`) for the next entry.
    /// Runs once before every entry; a no-op when the fabric is unwired.
    fn service_pending_irqs<B: Backend<A = Self>>(vmm: &mut Vmm<B>) -> Result<(), VmmError>;

    /// Complete delivery of every identity the backend accepted during the last
    /// entry (the fabric's pending → in-service transition).
    fn complete_irq_delivery<B: Backend<A = Self>>(vmm: &mut Vmm<B>);

    /// Whether the guest can currently take a maskable interrupt (x86:
    /// `RFLAGS.IF`) — the idle path's "a wake can reach the guest" gate. Reads
    /// the vCPU (a pure `Backend::save`, running no guest code); fails closed.
    fn guest_interruptible<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Result<bool, VmmError>;

    /// Whether a deliverable interrupt is **already pending** in the fabric (x86:
    /// a vector in the LAPIC IRR that arbitration would deliver).
    ///
    /// Peeks **without advancing** the fabric: the run loop advances it before
    /// every entry ([`service_pending_irqs`](Vendor::service_pending_irqs)), so at
    /// an idle exit it is already current, and advancing again here would be a
    /// second (V-time-identical, but gratuitous) tick on the snapshot path.
    fn pending_deliverable_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
    ) -> Result<bool, VmmError>;

    /// The next armed fabric-timer deadline in V-time ns, or `None` when no timer
    /// is armed (or no fabric is wired). Does not check deliverability.
    fn next_timer_deadline_vns<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Option<u64>;

    /// [`next_timer_deadline_vns`](Vendor::next_timer_deadline_vns), filtered to
    /// timers whose fire would actually deliver — an armed-but-undeliverable
    /// timer is no wake.
    fn deliverable_timer_deadline_vns<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Option<u64>;

    /// **Stage-time** validation of a wire-format
    /// [`InjectInterrupt`](environment::HostFault::InjectInterrupt) identity: can
    /// this machine deliver it at all, and is it a legal identity *for this
    /// vendor*?
    ///
    /// Which identities exist, which are reserved, and how wide the space is are
    /// all per-arch facts — x86's xAPIC has 8-bit vectors with `0..16` reserved,
    /// while a GIC's INTIDs run far past 255 and its `0..16` are perfectly
    /// deliverable SGIs. So the engine asks and the vendor answers; it must never
    /// bake one vendor's ranges into the control plane.
    fn check_wire_interrupt<B: Backend<A = Self>>(
        vmm: &Vmm<B>,
        vector: u32,
    ) -> Result<(), InterruptReject>;

    /// Raise the wire-format interrupt `vector` (a `u32` — identities are
    /// per-arch, ARCH-BOUNDARY §C) into the fabric so normal arbitration delivers
    /// it. Fails loud on an identity outside this vendor's range, or with no
    /// fabric wired.
    fn inject_wire_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        vector: u32,
    ) -> Result<(), VmmError>;

    /// Whether a genuine guest interrupt is pending delivery but not yet accepted
    /// (the architecturally in-flight event a synchronized snapshot may capture).
    /// Unlike [`pending_deliverable_interrupt`](Vendor::pending_deliverable_interrupt)
    /// this **advances** the fabric first (it is called from outside the run loop,
    /// where the fabric may be stale) and folds in the vendor's legacy lines. The
    /// advance is idempotent with the run loop's per-entry service, so it does not
    /// perturb a snapshot.
    fn has_pending_guest_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
    ) -> Result<bool, VmmError>;

    // --- serial --------------------------------------------------------------

    /// The serial output captured so far (the engine's `SERL` hash chunk, the run
    /// result, and the scrape stream all read this).
    fn serial_capture(devices: &Self::Devices) -> &[u8];

    /// Queue bytes on the guest's serial input (task 81 `exec`; off-record by
    /// ruling).
    fn inject_serial_input(devices: &mut Self::Devices, bytes: &[u8]);

    // --- state records (hash + snapshot) --------------------------------------

    /// The canonical byte encoding of the vCPU record set for the engine's `VCPU`
    /// hash chunk. Deterministic; canonicalizes exactly what the snapshot records
    /// canonicalize, so a restored VM hashes like a never-restored one.
    fn encode_vcpu_chunk(vcpu: &Self::VcpuState) -> Vec<u8>;

    /// The device residual-register bytes of the engine's `DEV\0` hash chunk (the
    /// engine appends its own terminal-reason bytes after them).
    fn encode_device_state(devices: &Self::Devices) -> Vec<u8>;

    /// Append the vendor's own device hash chunks (x86: `LAPC` + `LEGY`), in the
    /// vendor's fixed order, at the engine's fixed position in the blob.
    fn hash_device_chunks(devices: &Self::Devices, out: &mut Vec<u8>);

    /// The wire register view for the `regs` observation verb (task 80): which
    /// registers a machine *has* is per-arch, so the vendor fills the view. The
    /// engine supplies the `Moment`/V-time half (the one deterministic axis).
    fn regs_view(vcpu: &Self::VcpuState) -> RegsView;

    /// Append the vendor's per-component vCPU digests to the **diagnostic**
    /// [`Vmm::state_components`] breakdown (never part of `state_hash`), so a
    /// determinism bisector can localize which register file diverged.
    fn vcpu_components(vcpu: &Self::VcpuState, out: &mut Vec<(&'static str, [u8; 32])>);

    /// Whether the vCPU carries an event-injection record a quiescent-only codec
    /// would reject (the full task-39 set, inert residuals included).
    fn vcpu_has_inflight_injection(vcpu: &Self::VcpuState) -> bool;

    /// Whether the vCPU carries a **genuine** in-flight event (the active subset
    /// of [`vcpu_has_inflight_injection`](Vendor::vcpu_has_inflight_injection)).
    fn vcpu_has_active_injection(vcpu: &Self::VcpuState) -> bool;

    /// Fail if `vcpu` carries state the vendor's `vm_state` record subset cannot
    /// represent (sealing a lossy blob is worse than refusing it).
    fn check_sealable_vcpu(vcpu: &Self::VcpuState) -> Result<(), VmmError>;

    // -----------------------------------------------------------------------
    // The snapshot-state seam — a KNOWN, RULED DEFERRAL. Read this before adding
    // a vendor.
    //
    // The three hooks below are typed against the CONCRETE [`vm_state::VmState`],
    // whose `regs`/`sregs`/`xsave` records are x86-64's. They are therefore the one
    // place in this trait a second vendor CANNOT simply implement: an arm64 vendor
    // has no way to represent its register set without changing this signature
    // (an associated `type Snapshot`, or a vendor-parameterized `VmState`).
    //
    // This is **acknowledged and deferred to the ARM window** (`hm-cbt`), per the
    // pre-build ruling — not an oversight:
    //
    // - **The trait is designed, NOT frozen.** AA-3's trait-freeze memo owns the
    //   freeze decision, and the ruling explicitly accepts that pre-built code pays
    //   rework against the unfrozen trait. Inventing a vendor-associated snapshot
    //   abstraction *now* would mean designing it against zero real second
    //   consumers — exactly the speculative-generality the "spikes gate trust"
    //   posture exists to prevent.
    // - **The ARM record set is AA-6's MEASURED decision**, not a guess. Which
    //   sysregs a snapshot must carry (and which are latent) is precisely what the
    //   spike settles; a shape chosen before it would be a coin-flip that later
    //   constrains the real answer.
    // - **The wire format is ALREADY extensible** — that is what step 4 bought.
    //   `vm-state`'s TLV container, its version, and its **arch tag**
    //   (`VM_STATE_VERSION` 2 / `ARCH_X86_64`) are arch-neutral, and a foreign
    //   record set is rejected LOUDLY (`UnsupportedArch`) rather than
    //   reinterpreted. The *format* is ready for a second vendor; only this Rust
    //   type seam is pinned. The storage path is opaque already (the engine seals
    //   encoded bytes and never reads a record).
    //
    // **The CI arch gate cannot catch this class**, and should not be trusted to:
    // cross-checking for `aarch64-unknown-linux-gnu` proves the tree still compiles
    // with the x86 vendor `cfg`'d OUT, but no vendor exists there to *instantiate*
    // the trait, so a signature only a second vendor could refute stays invisible.
    // The structural check is the ARM skeleton itself (`hm-cbt`) — the first real
    // second implementor, which is the only thing that can prove the seam holds.
    // (A stub "dummy vendor" purely to force the check was considered; it arrives
    // for free with `hm-cbt`, which is a real one.)
    // -----------------------------------------------------------------------

    /// Build the canonical [`VmState`] from `vcpu` + the current machine (the
    /// memory-less half of a snapshot): the vendor record set, the device blob,
    /// and the contract hash; the engine's V-time/entropy block is read through
    /// the engine's `pub(crate)` surface. Infallible and byte-deterministic.
    ///
    /// **Pinned to x86's record set — see the deferral note above.** Vendor
    /// association of the snapshot state is reserved to the ARM window
    /// (`hm-cbt` / AA-6); the `vm-state` v2 arch tag is the format's extension
    /// point, and trait rework here is accepted by the pre-build ruling.
    fn build_vm_state<B: Backend<A = Self>>(vmm: &Vmm<B>, vcpu: &Self::VcpuState) -> VmState;

    /// Validate the vendor half of a [`VmState`] restore **without mutating
    /// anything**: the contract hash, the device blob, the event records, and the
    /// fabric/platform wiring coherence. (Pinned to x86's record set — see the
    /// deferral note above [`build_vm_state`](Vendor::build_vm_state).) Returns the decoded vCPU record set (with
    /// the restore-canonicalized events already applied), the guest clock-offset
    /// register the engine re-applies with its V-time commit, and the prepared
    /// device state for [`commit_restore`](Vendor::commit_restore).
    fn validate_restore<B: Backend<A = Self>>(
        vmm: &Vmm<B>,
        s: &VmState,
    ) -> Result<(Self::VcpuState, u64, Self::RestorePrep), VmmError>;

    /// Commit the vendor half of a validated restore (all infallible): install the
    /// prepared devices and the restored guest-observable output streams.
    fn commit_restore<B: Backend<A = Self>>(vmm: &mut Vmm<B>, prep: Self::RestorePrep);
}
