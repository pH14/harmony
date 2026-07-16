// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arm64 vendor's exit dispatch, interrupt-fabric seams, and snapshot
//! record glue (`docs/ARCH-BOUNDARY.md` §B's vendor column, arm64 row).
//!
//! Everything here names arm64: the PL011 device state, the `PSTATE.I`
//! interruptibility test, the sysreg-trap dispositions (fail-closed skeleton),
//! and the arm64 `vm_state` record set. The engine ([`crate::vmm`]) reaches
//! all of it **only** through the [`Vendor`](crate::vendor::Vendor) trait.
//!
//! **Skeleton posture, stated once:** the GICv3 fabric computes arbitration
//! and deadlines only — real *delivery* into a guest is `TODO(AA-6)` (the
//! vGICv3 round-trip verdict) and the boot roots leave it unwired; no MMIO
//! address is modeled (the machine memory map arrives with the M3 boot path);
//! and a trapped sysreg has no ruled disposition (`TODO(AA-6)`). Every one of
//! those absences **fails closed**, never silently succeeds — default-deny is
//! the posture the contract will fill in, not a stub to be papered over.

use hypercall_proto::Service;
use vm_state::Arm64VmState;
use vmm_backend::{Arm64, Arm64VcpuState, Backend, Gpa};

use crate::snapshot::SnapshotError;
use crate::vendor::InterruptReject;
use crate::vendor::arm64::contract;
use crate::vendor::arm64::devices::Pl011;
use crate::vendor::arm64::records::{self, Arm64DeviceState};
use crate::vmm::{Step, Vmm, VmmError};

/// `PSTATE.I` (IRQ mask, bit 7): set ⇒ maskable interrupts are masked — the
/// arm64 mirror of x86's `RFLAGS_IF` (inverted sense: masked vs enabled).
pub(crate) const PSTATE_I: u64 = 1 << 7;

/// `true` iff `addr` lies inside the `(base, len)` device frame.
fn in_frame(addr: u64, frame: (u64, u64)) -> bool {
    addr >= frame.0 && addr < frame.0 + frame.1
}

/// The arm64 per-VM device state
/// ([`Vendor::Devices`](crate::vendor::Vendor::Devices)): the PL011 UART
/// (always present — the serial console) and the optional GICv3 +
/// generic-timer fabric, mirroring x86's `lapic: Option<_>` wiring pattern.
pub struct Arm64Devices {
    /// The PL011 UART (serial console + the task-81 `exec` input queue).
    pub(crate) uart: Pl011,
    /// The userspace GICv3 + generic-timer model — the pure arbitration/
    /// deadline half of the fabric. **Its output is not delivered into a real
    /// guest**: the stock backend has no delivery path (M2 §Delivery;
    /// `TODO(AA-6)`, the vGICv3 round-trip verdict), so wiring it is a
    /// test/mock composition today, never a silicon claim.
    pub(crate) gic: Option<gicv3::Gicv3>,
}

impl Arm64Devices {
    /// Fresh (reset) arm64 device state: a reset PL011, no fabric.
    pub(crate) fn new() -> Self {
        Self {
            uart: Pl011::new(),
            gic: None,
        }
    }
}

impl<B: Backend<A = Arm64>> Vmm<B> {
    /// Service a trapped sysreg access ([`Arm64Exit::Sysreg`]
    /// (`vmm_backend::Arm64Exit::Sysreg`)). **Fails closed:** the sysreg
    /// dispositions are the ARM CPU contract's rows (`TODO(AA-6)`, the
    /// enforcement-mechanism truth table) and the trap surface itself is the
    /// AA-3 patched backend's (`TODO(patched-abi)`) — the skeleton rules no
    /// disposition, so a surfaced trap is a loud contract violation, never a
    /// silently invented value or a silently dropped write.
    pub(crate) fn dispatch_sysreg(
        &mut self,
        sysreg: u32,
        write: Option<u64>,
    ) -> Result<Step, VmmError> {
        let dir = if write.is_some() { "write" } else { "read" };
        Err(VmmError::ContractViolation(format!(
            "trapped sysreg {dir} ({sysreg:#010x}) has no ruled disposition: the arm64 \
             contract's sysreg rows are the AA-6 truth table's (and the trap surface is the \
             AA-3 patched backend's) — the skeleton fails closed"
        )))
    }

    /// Route an MMIO access over the [`board`](super::board) memory map: the
    /// PL011 console frame → the UART device; the reserved doorbell GPA → the
    /// hypercall doorbell (`docs/ARCH-BOUNDARY.md` §4: on arm64 a doorbell
    /// surfaces as `KVM_EXIT_MMIO`, recognized here — default-deny without an
    /// SDK channel, exactly as x86's `DOORBELL_PORT`); the GICv3 frames → the
    /// wired fabric, or a loud "GIC unwired (delivery AA-6-gated)" when it is
    /// not. Every other address fails closed (default-deny).
    pub(crate) fn dispatch_mmio_arm64(
        &mut self,
        gpa: Gpa,
        size: u8,
        write: Option<u64>,
    ) -> Result<Step, VmmError> {
        use super::board::{DOORBELL, GICD, GICR, PL011};

        let addr = gpa.0;

        // Validate any access whose START lands in a modeled device frame
        // **fully**, before touching device state — a start-in-frame predicate
        // alone is unsafe (`in_frame` checks the start only). Every modeled
        // arm64 device (the PL011 register block, the GICv3 register files, the
        // hypercall-doorbell magic word) is a 32-bit-register/word-ABI, so the
        // access must be (1) 4-byte **aligned** (register-addressed), (2) fully
        // **within** the frame — its end must not straddle the boundary — and
        // (3) exactly 4 bytes **wide**. Anything else fails closed (never a
        // silent `v as u32` truncation, an under-filled load, or a cross-frame
        // access). Checked in that order so a straddling access is reported as a
        // straddle even when its width is also wrong.
        if let Some((frame_name, frame)) = [
            ("PL011", PL011),
            ("doorbell", DOORBELL),
            ("GICD", GICD),
            ("GICR", GICR),
        ]
        .into_iter()
        .find(|(_, f)| in_frame(addr, *f))
        {
            if !addr.is_multiple_of(4) {
                return Err(VmmError::ContractViolation(format!(
                    "arm64 {frame_name} MMIO at {addr:#x} is not 4-byte aligned — the modeled \
                     registers are word-addressed; a misaligned access is unmodeled (fail closed)"
                )));
            }
            let end = addr.checked_add(u64::from(size));
            if end.is_none_or(|e| e > frame.0 + frame.1) {
                return Err(VmmError::ContractViolation(format!(
                    "arm64 {frame_name} MMIO at {addr:#x} size {size} straddles the frame boundary \
                     ({:#x}..{:#x}) — a cross-frame access is unmodeled (fail closed)",
                    frame.0,
                    frame.0 + frame.1
                )));
            }
            if size != 4 {
                return Err(VmmError::ContractViolation(format!(
                    "arm64 {frame_name} MMIO at {addr:#x} with size {size} != 4 — the modeled \
                     registers are 32-bit-accessed; a different width is unmodeled (fail closed, \
                     not a truncation)"
                )));
            }
        }

        // The PL011 console (4 KiB frame). 32-bit register accesses.
        if in_frame(addr, PL011) {
            let offset = addr - PL011.0;
            return match write {
                None => {
                    let v = self.devices.uart.read(offset);
                    self.backend.complete_read(u64::from(v))?;
                    Ok(Step::Continued)
                }
                Some(v) => {
                    self.devices.uart.write(offset, v as u32);
                    Ok(Step::Continued)
                }
            };
        }

        // The hypercall doorbell (reserved MMIO GPA). A store rings it; the
        // dispatcher default-denies a service this composition does not offer.
        if in_frame(addr, DOORBELL) {
            let Some(v) = write else {
                return Err(VmmError::ContractViolation(format!(
                    "load from the hypercall doorbell GPA {addr:#x}: the doorbell is a store-only \
                     ring (a request-page GPA), never read"
                )));
            };
            return self.service_doorbell(v as u32);
        }

        // The GICv3 distributor / redistributor frames (width already checked
        // above — the modeled register files are 32-bit-accessed; 64-bit GIC
        // registers like IROUTERn/GICR_TYPER are unmodeled, `TODO(AA-6)`).
        if in_frame(addr, GICD) || in_frame(addr, GICR) {
            let (frame, base) = if in_frame(addr, GICD) {
                (gicv3::GicFrame::Dist, GICD.0)
            } else {
                (gicv3::GicFrame::Redist, GICR.0)
            };
            if self.devices.gic.is_none() {
                return Err(VmmError::ContractViolation(format!(
                    "GICv3 MMIO at {addr:#x} but the userspace GICv3 is unwired — guest \
                     delivery is AA-6-gated (the in-kernel vGICv3 round-trip verdict); a \
                     stock-backend boot never wires it"
                )));
            }
            let now_vns = self.now_vns()?;
            let offset = addr - base;
            let gic = self.devices.gic.as_mut().expect("is_none checked above");
            return match write {
                None => {
                    let v = gic.mmio_read(frame, offset, now_vns).map_err(|e| {
                        VmmError::ContractViolation(format!("GICv3 read {offset:#x}: {e}"))
                    })?;
                    self.backend.complete_read(u64::from(v))?;
                    Ok(Step::Continued)
                }
                Some(v) => {
                    gic.mmio_write(frame, offset, v as u32, now_vns)
                        .map_err(|e| {
                            VmmError::ContractViolation(format!("GICv3 write {offset:#x}: {e}"))
                        })?;
                    Ok(Step::Continued)
                }
            };
        }

        Err(VmmError::ContractViolation(format!(
            "unmodeled MMIO at {addr:#x} (size {size}); only the PL011 console, the GICv3 \
             frames, and the hypercall doorbell are modeled on the arm64 board"
        )))
    }

    /// Wire the userspace GICv3 + generic-timer fabric. **Arbitration and
    /// deadlines only** (`tasks/112` M2): the model's output feeds the
    /// backend's one-slot inject seam, which the **stock** `Arm64KvmBackend`
    /// answers `Unsupported` (no delivery path into a real guest exists for a
    /// userspace GIC — `TODO(AA-6)`, the vGICv3 round-trip verdict). Wiring is
    /// therefore a mock/test composition; the arm64 boot roots leave it
    /// unwired.
    pub fn wire_gic(&mut self, gic: gicv3::Gicv3) -> &mut Self {
        self.devices.gic = Some(gic);
        self
    }

    /// `true` once the userspace GICv3 is wired.
    pub fn gic_wired(&self) -> bool {
        self.devices.gic.is_some()
    }

    /// Advance the fabric to the current V-time and hand the backend the one
    /// arbitrated deliverable INTID (or `None`) for the next entry. Peeking
    /// (not taking) leaves it pending; the pending→active transition happens
    /// in [`Self::complete_irq_delivery_arm64`] only once the backend confirms
    /// acceptance — the same discipline as x86's LAPIC path, so a snapshot
    /// taken while an INTID awaits injection shows it pending. A no-op when
    /// the fabric is unwired (the x86 unwired-LAPIC posture: the backend's
    /// inject seam is never touched and `state_hash` carries no fabric chunk).
    pub(crate) fn service_pending_irqs_arm64(&mut self) -> Result<(), VmmError> {
        if self.devices.gic.is_none() {
            return Ok(());
        }
        let now_vns = self.now_vns()?;
        let intid = {
            let gic = self.devices.gic.as_mut().expect("is_some checked above");
            gic.advance_to(now_vns);
            gic.peek_interrupt() // re-arbitrate; do NOT move pending→active
        };
        self.backend
            .set_pending_irq(intid.map(vmm_backend::GicIntId))?;
        Ok(())
    }

    /// Complete delivery of every INTID the backend accepted during the last
    /// entry: the fabric's pending→active transition. With no fabric wired
    /// the accepted queue is still drained so a mock-injected identity can
    /// never sit stale across entries.
    pub(crate) fn complete_irq_delivery_arm64(&mut self) {
        while self.backend.take_accepted_interrupt().is_some() {
            if let Some(gic) = self.devices.gic.as_mut() {
                gic.take_interrupt();
            }
        }
    }

    /// Whether a deliverable interrupt is **already pending** in the fabric.
    /// Peeks without advancing (the run loop advances before every entry, so
    /// at an idle exit the fabric is already current). No fabric ⇒ never.
    pub(crate) fn pending_deliverable_interrupt_arm64(&mut self) -> Result<bool, VmmError> {
        Ok(self
            .devices
            .gic
            .as_ref()
            .is_some_and(|g| g.peek_interrupt().is_some()))
    }

    /// The next armed generic-timer deadline in V-time ns (the pure
    /// deadlines-out half of the fabric). No fabric ⇒ none.
    pub(crate) fn next_timer_deadline_vns_arm64(&self) -> Option<u64> {
        self.devices.gic.as_ref()?.next_timer_deadline()
    }

    /// [`Self::next_timer_deadline_vns_arm64`], filtered to timers whose fire
    /// would actually deliver — an armed-but-undeliverable timer is no wake.
    pub(crate) fn deliverable_timer_deadline_vns_arm64(&self) -> Option<u64> {
        let gic = self.devices.gic.as_ref()?;
        gic.next_timer_deadline()
            .filter(|_| gic.armed_timer_deliverable())
    }

    /// Stage-time validation of a wire-format interrupt identity against the
    /// **implemented, distributor-bounded** GICv3 identity space: SGIs `0..16`
    /// are deliverable (never x86's reserved-vector rule), PPIs `16..32`, SPIs
    /// to the configured limit; anything past the implemented range is
    /// [`InterruptReject::OutOfRange`]. No fabric ⇒
    /// [`InterruptReject::NoFabric`].
    pub(crate) fn check_wire_interrupt_arm64(&self, vector: u32) -> Result<(), InterruptReject> {
        let Some(gic) = self.devices.gic.as_ref() else {
            return Err(InterruptReject::NoFabric);
        };
        if !gic.implemented(vector) {
            return Err(InterruptReject::OutOfRange);
        }
        Ok(())
    }

    /// Raise the wire-format INTID pending in the fabric so normal arbitration
    /// delivers it. Fails loud on an unimplemented identity or with no fabric
    /// wired (guest delivery itself stays AA-6-gated — see
    /// [`Self::wire_gic`]).
    pub(crate) fn inject_host_interrupt_arm64(&mut self, vector: u32) -> Result<(), VmmError> {
        let Some(gic) = self.devices.gic.as_mut() else {
            return Err(VmmError::ContractViolation(format!(
                "InjectInterrupt INTID {vector:#x} but no arm64 delivery fabric is wired — the \
                 GICv3 arbitration model is unwired in this composition and guest delivery is \
                 AA-6-gated (the in-kernel vGICv3 round-trip verdict)"
            )));
        };
        gic.raise(vector).map_err(|e| {
            VmmError::ContractViolation(format!("InjectInterrupt INTID {vector:#x} rejected: {e}"))
        })
    }

    /// Whether a genuine guest interrupt is pending delivery but not yet
    /// accepted. Advances the fabric first (this is called from outside the
    /// run loop, where the fabric may be stale; the advance is idempotent with
    /// the per-entry service). No fabric ⇒ never.
    pub(crate) fn has_pending_guest_interrupt_arm64(&mut self) -> Result<bool, VmmError> {
        if self.devices.gic.is_none() {
            return Ok(false);
        }
        let now_vns = self.now_vns()?;
        let gic = self.devices.gic.as_mut().expect("is_some checked above");
        gic.advance_to(now_vns);
        Ok(gic.peek_interrupt().is_some())
    }

    /// Build the canonical [`Arm64VmState`] from `vcpu` + the current live
    /// machine (the memory-less half of a snapshot): the arm64 record set, the
    /// V-time block + entropy stream, and the vmm-core-owned device blob
    /// (PL011 residuals, the report stream, the guest clock offset). The
    /// `contract_hash` is stamped so a restore can reject a blob taken under a
    /// different policy skeleton. Infallible and byte-deterministic — the
    /// V-time block anchors to the deterministic `last_intercept_work`,
    /// exactly like the x86 builder.
    pub(crate) fn build_vm_state_arm64(&self, vcpu: &Arm64VcpuState) -> Arm64VmState {
        let mut s = Arm64VmState::default();
        records::fill_vcpu_state(&mut s, vcpu);
        let clock_offset = match &self.vtime {
            Some(vt) => {
                s.vtime = vm_state::VtimeState {
                    ratio_num: vt.cfg.ratio_num,
                    // `VtimeWiring::new` enforces `ratio_den == 1`; carry it so
                    // the blob is encodable.
                    ratio_den: 1,
                    guest_hz: vt.cfg.guest_hz,
                    guest_base: vt.cfg.guest_base,
                    snapshot_vns: vt.clock.snapshot_vns(vt.last_intercept_work),
                };
                s.hypercall = vt.entropy.save_state();
                vt.guest_clock_offset
            }
            None => {
                // Unwired: a sentinel encodable V-time block, no entropy.
                s.vtime.ratio_den = 1;
                0
            }
        };
        let dev = Arm64DeviceState {
            clock_offset,
            report_stream: self.report_stream.clone(),
            uart_capture: self.devices.uart.capture().to_vec(),
            uart_regs: *self.devices.uart.shadow_regs(),
            gic: self.devices.gic.as_ref().map(|g| g.snapshot()),
            // The dedicated hypercall-transport ABI pages ride the blob so
            // save/restore/branch preserve them (they are a separate memslot, not
            // in the main-RAM snapshot). Empty when the VM never mapped them.
            doorbell: self
                .doorbell_pages
                .as_ref()
                .map(|db| db.as_bytes().to_vec())
                .unwrap_or_default(),
        };
        s.devices = records::encode_device_blob(&dev);
        s.contract_hash = contract::contract_hash();
        s
    }

    /// The arm64 half of a snapshot restore, **validating without mutating**:
    /// the contract hash, the device blob, and the channel composition. Yields
    /// the decoded vCPU record set, the guest clock-offset register the engine
    /// re-applies with its V-time commit, and the prepared device state for
    /// [`Self::commit_restore_arm64`].
    pub(crate) fn validate_restore_arm64(
        &self,
        s: &Arm64VmState,
    ) -> Result<(Arm64VcpuState, u64, Arm64RestorePrep), VmmError> {
        // A blob taken under a different policy skeleton would silently
        // diverge on restore (the x86 `contract_hash` discipline).
        if s.contract_hash != contract::contract_hash() {
            return Err(VmmError::Snapshot(SnapshotError::ContractMismatch));
        }
        // Decode the vmm-core device blob (total, never panics).
        let dev = records::decode_device_blob(&s.devices.0)?;
        // The blob's GICv3 record must be coherent AND match this VM's wiring
        // (the x86 LAPIC wiring-mismatch discipline): one side having a fabric
        // the other lacks would silently change which interrupts can ever
        // deliver — rejected, never skipped.
        let new_gic = match (&dev.gic, self.devices.gic.as_ref()) {
            (Some(gs), Some(target)) => {
                // The snapshot's GIC **config** (impl_spis / timer_hz /
                // timer_intid) must match the already-wired target's — these
                // drive `GICD_TYPER.ITLinesNumber` and the tick→ns deadline
                // conversion, so adopting the blob's config under an unchanged
                // board/DTB contract would silently change the machine the
                // guest sees. Reject a mismatch (the LAPIC wiring-mismatch
                // posture), never a silent adoption.
                let have = target.config();
                if (gs.impl_spis, gs.timer_hz, gs.timer_intid)
                    != (have.impl_spis, have.timer_hz, have.timer_intid)
                {
                    return Err(VmmError::ContractViolation(format!(
                        "restore_vm_state: GICv3 config mismatch (snapshot impl_spis={} timer_hz={} \
                         timer_intid={} vs this VM's {}/{}/{}) — the distributor bound and the \
                         timer deadline conversion cannot change under an unchanged board/DTB; \
                         restore into a VM composed like the snapshot source.",
                        gs.impl_spis,
                        gs.timer_hz,
                        gs.timer_intid,
                        have.impl_spis,
                        have.timer_hz,
                        have.timer_intid
                    )));
                }
                // Validate the GIC's one-shot timer latch against the snapshot's
                // sealed V-time (`VtimeState::snapshot_vns`) — a fired latch with
                // a future deadline is a state the model never produces.
                Some(
                    gicv3::Gicv3::restore(gs, s.vtime.snapshot_vns).map_err(|_| {
                        SnapshotError::DeviceRestore("incoherent GicState in device blob")
                    })?,
                )
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(VmmError::ContractViolation(
                    "restore_vm_state: snapshot/VM GICv3 wiring mismatch (one has the fabric, \
                     the other does not) — restore into a VM composed like the snapshot source."
                        .to_string(),
                ));
            }
            (None, None) => None,
        };
        // The dedicated hypercall-transport ABI pages must match this VM's wiring
        // (the GIC wiring-mismatch discipline): a snapshot that carries them
        // restored into a VM without the memslot — or vice versa — would silently
        // drop or misplace guest-visible transport state. When both have them the
        // lengths must agree (both `2 · HC_PAGE`).
        match self.doorbell_pages.as_ref() {
            Some(db) if !dev.doorbell.is_empty() => {
                if dev.doorbell.len() != db.len() {
                    return Err(VmmError::ContractViolation(format!(
                        "restore_vm_state: doorbell-pages length mismatch (snapshot {} vs this \
                         VM's {}) — restore into a VM composed like the snapshot source.",
                        dev.doorbell.len(),
                        db.len()
                    )));
                }
            }
            None if dev.doorbell.is_empty() => {}
            _ => {
                return Err(VmmError::ContractViolation(
                    "restore_vm_state: hypercall-transport doorbell wiring mismatch (one side \
                     mapped the dedicated ABI pages, the other did not) — restore into a VM \
                     composed like the snapshot source."
                        .to_string(),
                ));
            }
        }
        // The arm64 skeleton blob carries **no pvclock channel record** (the
        // arm64 clock-page protocol is `hm-rk5`'s; this skeleton only reserves
        // the seam). Validate that symmetrically against this VM's
        // composition: a pvclock-wired restore target fails loud rather than
        // silently forking the sealed timeline.
        self.pvclock_validate_restore(None)?;
        let vcpu = records::vcpu_state_from(s);
        let clock_offset = dev.clock_offset;
        Ok((vcpu, clock_offset, Arm64RestorePrep { gic: new_gic, dev }))
    }

    /// The arm64 half of the restore **commit** (all infallible): install the
    /// coherence-checked GICv3, the PL011 residual state, and the restored
    /// guest-observable report stream.
    pub(crate) fn commit_restore_arm64(&mut self, prep: Arm64RestorePrep) {
        let Arm64RestorePrep { gic, dev } = prep;
        if let Some(g) = gic {
            self.devices.gic = Some(g);
        }
        self.devices.uart.restore(dev.uart_capture, dev.uart_regs);
        // Restore the dedicated transport ABI pages (validate_restore already
        // checked the wiring + length, so this is infallible).
        if let Some(db) = self
            .doorbell_pages
            .as_mut()
            .filter(|_| !dev.doorbell.is_empty())
        {
            db.as_mut_bytes().copy_from_slice(&dev.doorbell);
        }
        self.pvclock_commit_restore(None);
        self.report_stream = dev.report_stream;
    }
}

/// The arm64 half of a validated-but-uncommitted restore
/// ([`Vendor::validate_restore`](crate::vendor::Vendor::validate_restore) →
/// [`Vendor::commit_restore`](crate::vendor::Vendor::commit_restore)): the
/// coherence-checked GICv3 and the decoded device blob.
pub struct Arm64RestorePrep {
    gic: Option<gicv3::Gicv3>,
    dev: Arm64DeviceState,
}

/// Deterministic, fixed-layout encoding of an [`Arm64VcpuState`] for the
/// engine's `VCPU` hash chunk (little-endian, declaration order; no map
/// iteration, no float). Canonicalizes exactly what the snapshot records
/// canonicalize, so a restored VM hashes like a never-restored one.
pub(crate) fn encode_vcpu_state(s: &Arm64VcpuState) -> Vec<u8> {
    let mut v = Vec::new();
    for x in s.core.x {
        v.extend_from_slice(&x.to_le_bytes());
    }
    for x in [
        s.core.sp,
        s.core.pc,
        s.core.pstate,
        s.core.sp_el1,
        s.core.elr_el1,
        s.core.spsr_el1,
    ] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    for x in [
        s.sysregs.sctlr_el1,
        s.sysregs.ttbr0_el1,
        s.sysregs.ttbr1_el1,
        s.sysregs.tcr_el1,
        s.sysregs.mair_el1,
        s.sysregs.vbar_el1,
        s.sysregs.cpacr_el1,
        s.sysregs.esr_el1,
        s.sysregs.far_el1,
        s.sysregs.tpidr_el0,
        s.sysregs.tpidr_el1,
        s.sysregs.cntkctl_el1,
    ] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.push(match s.mp_state {
        vmm_backend::MpState::Runnable => 0,
        vmm_backend::MpState::Halted => 1,
    });
    v
}

/// SHA-256 of `bytes`, for the diagnostic component digests.
fn dig(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// The arm64 **device** breakdown for the diagnostic [`Vmm::state_components`]
/// (never part of `state_hash`): the `gic` component digests exactly the bytes
/// the `GICV` hash chunk hashes ([`Arm64::hash_device_chunks`](crate::vendor::Vendor::hash_device_chunks)),
/// so a GIC-only `state_hash` divergence localizes here. Present only when the
/// fabric is wired (an unwired VM has no `GICV` chunk either).
pub(crate) fn device_components(devices: &Arm64Devices, out: &mut Vec<(&'static str, [u8; 32])>) {
    if let Some(gic) = &devices.gic {
        let mut bytes = Vec::new();
        records::encode_gic_state(&mut bytes, &gic.snapshot());
        out.push(("gic", dig(&bytes)));
    }
}

/// The arm64 register-file breakdown for the **diagnostic**
/// [`Vmm::state_components`] (never part of `state_hash`), so a determinism
/// bisector can localize which register file diverged. Labels are stable and
/// in a fixed order (the arm64 vendor's own label set — the x86 labels
/// `regs`/`desc-tables`/… are that vendor's and stay untouched).
pub(crate) fn vcpu_components(s: &Arm64VcpuState, out: &mut Vec<(&'static str, [u8; 32])>) {
    let mut core = Vec::new();
    for x in s.core.x {
        core.extend_from_slice(&x.to_le_bytes());
    }
    for x in [
        s.core.sp,
        s.core.pc,
        s.core.pstate,
        s.core.sp_el1,
        s.core.elr_el1,
        s.core.spsr_el1,
    ] {
        core.extend_from_slice(&x.to_le_bytes());
    }
    out.push(("core-regs", dig(&core)));

    let mut sys = Vec::new();
    for x in [
        s.sysregs.sctlr_el1,
        s.sysregs.ttbr0_el1,
        s.sysregs.ttbr1_el1,
        s.sysregs.tcr_el1,
        s.sysregs.mair_el1,
        s.sysregs.vbar_el1,
        s.sysregs.cpacr_el1,
        s.sysregs.esr_el1,
        s.sysregs.far_el1,
        s.sysregs.tpidr_el0,
        s.sysregs.tpidr_el1,
        s.sysregs.cntkctl_el1,
    ] {
        sys.extend_from_slice(&x.to_le_bytes());
    }
    out.push(("sysregs", dig(&sys)));

    let mp = match s.mp_state {
        vmm_backend::MpState::Runnable => 0u8,
        vmm_backend::MpState::Halted => 1,
    };
    out.push(("mp_state", dig(&[mp])));
}
