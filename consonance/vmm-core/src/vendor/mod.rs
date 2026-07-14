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

    /// Whether this machine can enforce a wire-vector
    /// [`InjectInterrupt`](environment::HostFault::InjectInterrupt) host fault at
    /// all (x86: the userspace LAPIC is wired). Stage-time validation.
    fn can_inject_wire_interrupt<B: Backend<A = Self>>(vmm: &Vmm<B>) -> bool;

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

    /// Build the canonical [`VmState`] from `vcpu` + the current machine (the
    /// memory-less half of a snapshot): the vendor record set, the device blob,
    /// and the contract hash; the engine's V-time/entropy block is read through
    /// the engine's `pub(crate)` surface. Infallible and byte-deterministic.
    fn build_vm_state<B: Backend<A = Self>>(vmm: &Vmm<B>, vcpu: &Self::VcpuState) -> VmState;

    /// Validate the vendor half of a [`VmState`] restore **without mutating
    /// anything**: the contract hash, the device blob, the event records, and the
    /// fabric/platform wiring coherence. Returns the decoded vCPU record set (with
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
