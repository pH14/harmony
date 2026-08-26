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

// Canonical HVF trapped-system-register ISS identities: architectural op
// fields with Rt and direction cleared (`vmm-backend::hvf`). These are pinned
// by the signed M1 probe and are independent of the instruction's source/dest
// register.
const ICC_PMR_EL1: u32 = 0x0030_100c;
const ICC_IAR1_EL1: u32 = 0x0030_3018;
const ICC_EOIR1_EL1: u32 = 0x0032_3018;
const ICC_IGRPEN1_EL1: u32 = 0x003e_3018;
const ICC_BPR1_EL1: u32 = 0x0036_3018;
const ICC_CTLR_EL1: u32 = 0x0038_3018;
const ICC_RPR_EL1: u32 = 0x0036_3016;
const ICC_HPPIR1_EL1: u32 = 0x0034_3018;

/// `true` iff `addr` lies inside the `(base, len)` device frame.
fn in_frame(addr: u64, frame: (u64, u64)) -> bool {
    addr >= frame.0 && addr < frame.0 + frame.1
}

/// Whether `addr` is an implemented SPI's 64-bit `GICD_IROUTERn` register.
/// The uniprocessor model has one fixed affinity target (Aff0..3 = 0), so the
/// register is stateless and its only supported value is zero.
fn is_gicd_irouter(addr: u64) -> bool {
    use super::board::{GICD, IMPL_SPIS};

    const IROUTER_BASE: u64 = 0x6000;
    let first_spi = GICD.0 + IROUTER_BASE + 32 * 8;
    let end = first_spi + u64::from(IMPL_SPIS) * 8;
    (first_spi..end).contains(&addr) && addr.is_multiple_of(8)
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
        let ruled = matches!(
            sysreg,
            ICC_PMR_EL1
                | ICC_IAR1_EL1
                | ICC_EOIR1_EL1
                | ICC_IGRPEN1_EL1
                | ICC_BPR1_EL1
                | ICC_CTLR_EL1
                | ICC_RPR_EL1
                | ICC_HPPIR1_EL1
        );
        if !ruled {
            let dir = if write.is_some() { "write" } else { "read" };
            return Err(VmmError::ContractViolation(format!(
                "trapped sysreg {dir} ({sysreg:#010x}) has no ruled disposition for HVF"
            )));
        }
        let Some(gic) = self.devices.gic.as_mut() else {
            return Err(VmmError::ContractViolation(format!(
                "trapped GIC CPU-interface sysreg {sysreg:#010x} with no userspace GIC wired"
            )));
        };
        match (sysreg, write) {
            (ICC_IAR1_EL1, None) => {
                let intid = gic
                    .active_interrupt()
                    .unwrap_or(vmm_backend::GicIntId::SPURIOUS.0);
                self.backend.complete_read(u64::from(intid))?;
                Ok(Step::Continued)
            }
            (ICC_EOIR1_EL1, Some(intid)) => {
                let intid = u32::try_from(intid).map_err(|_| {
                    VmmError::ContractViolation("ICC_EOIR1_EL1 INTID exceeds u32".to_string())
                })?;
                gic.eoi(intid).map_err(|e| {
                    VmmError::ContractViolation(format!("ICC_EOIR1_EL1 rejected: {e}"))
                })?;
                self.backend.complete_ok()?;
                Ok(Step::Continued)
            }
            (ICC_PMR_EL1, None) => {
                self.backend.complete_read(u64::from(gic.pmr()))?;
                Ok(Step::Continued)
            }
            (ICC_PMR_EL1, Some(value)) => {
                let pmr = u8::try_from(value).map_err(|_| {
                    VmmError::ContractViolation("ICC_PMR_EL1 value exceeds u8".to_string())
                })?;
                gic.set_pmr(pmr);
                self.backend.complete_ok()?;
                Ok(Step::Continued)
            }
            // The model is single-security-state Group-1-only. The binary
            // point/priority controls Linux writes are accepted at their only
            // supported values; reads return that fixed interface shape.
            (ICC_IGRPEN1_EL1, Some(value)) if value <= 1 => {
                self.backend.complete_ok()?;
                Ok(Step::Continued)
            }
            (ICC_BPR1_EL1 | ICC_CTLR_EL1, Some(0)) => {
                self.backend.complete_ok()?;
                Ok(Step::Continued)
            }
            (ICC_IGRPEN1_EL1, None) => {
                self.backend.complete_read(1)?;
                Ok(Step::Continued)
            }
            (ICC_BPR1_EL1 | ICC_CTLR_EL1, None) => {
                self.backend.complete_read(0)?;
                Ok(Step::Continued)
            }
            (ICC_RPR_EL1, None) => {
                self.backend.complete_read(0xff)?;
                Ok(Step::Continued)
            }
            (ICC_HPPIR1_EL1, None) => {
                let intid = gic
                    .peek_interrupt()
                    .unwrap_or(vmm_backend::GicIntId::SPURIOUS.0);
                self.backend.complete_read(u64::from(intid))?;
                Ok(Step::Continued)
            }
            _ => {
                let dir = if write.is_some() { "write" } else { "read" };
                Err(VmmError::ContractViolation(format!(
                    "trapped GIC CPU-interface sysreg {dir} ({sysreg:#010x}, value={write:?}) \
                     has no ruled value"
                )))
            }
        }
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
        // arm64 device is range-checked before touching state. GIC and the
        // doorbell are strict 32-bit word ABIs. PL011 registers remain
        // word-addressed but architecturally admit 8/16/32-bit transfers at the
        // register base; Linux earlycon uses an 8-bit UARTDR store. The one
        // 64-bit GIC access modeled here is GICR_TYPER at offset 0x8, whose
        // architectural width Linux uses while discovering redistributors.
        // Anything else fails closed (never a silent truncation or cross-frame
        // access).
        if let Some((frame_name, frame)) = [
            ("PL011", PL011),
            ("doorbell", DOORBELL),
            ("GICD", GICD),
            ("GICR", GICR),
        ]
        .into_iter()
        .find(|(_, f)| in_frame(addr, *f))
        {
            let end = addr.checked_add(u64::from(size));
            if end.is_none_or(|e| e > frame.0 + frame.1) {
                return Err(VmmError::ContractViolation(format!(
                    "arm64 {frame_name} MMIO at {addr:#x} size {size} straddles the frame boundary \
                     ({:#x}..{:#x}) — a cross-frame access is unmodeled (fail closed)",
                    frame.0,
                    frame.0 + frame.1
                )));
            }
            if !addr.is_multiple_of(4) {
                return Err(VmmError::ContractViolation(format!(
                    "arm64 {frame_name} MMIO at {addr:#x} is not 4-byte aligned — the modeled \
                     registers are word-addressed; a misaligned access is unmodeled (fail closed)"
                )));
            }
            let valid_width = match frame_name {
                "PL011" => matches!(size, 1 | 2 | 4),
                "GICR" => size == 4 || (size == 8 && addr - frame.0 == 0x8),
                "GICD" => size == 4 || (size == 8 && is_gicd_irouter(addr)),
                _ => size == 4,
            };
            if !valid_width {
                return Err(VmmError::ContractViolation(format!(
                    "arm64 {frame_name} MMIO at {addr:#x} has unmodeled size {size} \
                     (fail closed)"
                )));
            }
        }

        // The PL011 console (4 KiB frame). Values occupy the low transfer
        // bytes; mask explicitly so synthetic backends cannot smuggle high
        // bits that real HVF already truncates at the MMIO exit.
        if in_frame(addr, PL011) {
            let offset = addr - PL011.0;
            let bits = u32::from(size) * 8;
            let mask = if bits == 32 {
                u32::MAX
            } else {
                (1u32 << bits) - 1
            };
            return match write {
                None => {
                    let v = self.devices.uart.read(offset) & mask;
                    self.backend.complete_read(u64::from(v))?;
                    Ok(Step::Continued)
                }
                Some(v) => {
                    self.devices.uart.write(offset, v as u32 & mask);
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
        // above). GICR_TYPER is composed from its two 32-bit halves so the
        // device model retains one canonical register-access primitive.
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
            // GICD_IROUTERn is 64-bit. This uniprocessor machine exposes only
            // affinity zero, so it reads zero and accepts exactly zero. A
            // nonzero affinity would promise routing the model cannot perform.
            if size == 8 && is_gicd_irouter(addr) {
                return match write {
                    None => {
                        self.backend.complete_read(0)?;
                        Ok(Step::Continued)
                    }
                    Some(0) => Ok(Step::Continued),
                    Some(value) => Err(VmmError::ContractViolation(format!(
                        "GICD_IROUTER at {addr:#x} requests unsupported affinity {value:#x}; \
                         the single-vCPU machine routes only to affinity zero"
                    ))),
                };
            }
            let now_vns = self.now_vns()?;
            let offset = addr - base;
            let gic = self.devices.gic.as_mut().expect("is_none checked above");
            return match write {
                None => {
                    let lo = gic.mmio_read(frame, offset, now_vns).map_err(|e| {
                        VmmError::ContractViolation(format!("GICv3 read {offset:#x}: {e}"))
                    })?;
                    let value = if size == 8 {
                        let hi_offset = offset.checked_add(4).ok_or_else(|| {
                            VmmError::ContractViolation(
                                "GICv3 64-bit read offset overflow".to_string(),
                            )
                        })?;
                        let hi = gic.mmio_read(frame, hi_offset, now_vns).map_err(|e| {
                            VmmError::ContractViolation(format!("GICv3 read {hi_offset:#x}: {e}"))
                        })?;
                        u64::from(lo) | (u64::from(hi) << 32)
                    } else {
                        u64::from(lo)
                    };
                    self.backend.complete_read(value)?;
                    Ok(Step::Continued)
                }
                Some(v) => {
                    if size == 8 {
                        return Err(VmmError::ContractViolation(format!(
                            "arm64 GICR MMIO write at {addr:#x} has unmodeled 64-bit direction \
                             (GICR_TYPER is read-only; fail closed)"
                        )));
                    }
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
    for q in s.simd_fp.q {
        v.extend_from_slice(&q);
    }
    v.extend_from_slice(&s.simd_fp.fpcr.to_le_bytes());
    v.extend_from_slice(&s.simd_fp.fpsr.to_le_bytes());
    for file in [
        s.debug.breakpoint_value,
        s.debug.breakpoint_control,
        s.debug.watchpoint_value,
        s.debug.watchpoint_control,
    ] {
        for value in file {
            v.extend_from_slice(&value.to_le_bytes());
        }
    }
    v.extend_from_slice(&s.debug.mdscr_el1.to_le_bytes());
    v.push(u8::from(s.debug.trap_debug_exceptions));
    v.push(u8::from(s.debug.trap_debug_reg_accesses));
    v.extend_from_slice(&s.vtimer.cntv_ctl_el0.to_le_bytes());
    v.extend_from_slice(&s.vtimer.cntv_cval_el0.to_le_bytes());
    v.push(u8::from(s.vtimer.masked));
    v.extend_from_slice(&s.vtimer.offset.to_le_bytes());
    v.push(u8::from(s.interrupts.irq));
    v.push(u8::from(s.interrupts.fiq));
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

    let mut simd_fp = Vec::new();
    for q in s.simd_fp.q {
        simd_fp.extend_from_slice(&q);
    }
    simd_fp.extend_from_slice(&s.simd_fp.fpcr.to_le_bytes());
    simd_fp.extend_from_slice(&s.simd_fp.fpsr.to_le_bytes());
    out.push(("simd-fp", dig(&simd_fp)));

    let mut debug = Vec::new();
    for file in [
        s.debug.breakpoint_value,
        s.debug.breakpoint_control,
        s.debug.watchpoint_value,
        s.debug.watchpoint_control,
    ] {
        for value in file {
            debug.extend_from_slice(&value.to_le_bytes());
        }
    }
    debug.extend_from_slice(&s.debug.mdscr_el1.to_le_bytes());
    debug.push(u8::from(s.debug.trap_debug_exceptions));
    debug.push(u8::from(s.debug.trap_debug_reg_accesses));
    out.push(("debug", dig(&debug)));

    let mut vtimer = Vec::new();
    vtimer.extend_from_slice(&s.vtimer.cntv_ctl_el0.to_le_bytes());
    vtimer.extend_from_slice(&s.vtimer.cntv_cval_el0.to_le_bytes());
    vtimer.push(u8::from(s.vtimer.masked));
    vtimer.extend_from_slice(&s.vtimer.offset.to_le_bytes());
    out.push(("vtimer", dig(&vtimer)));

    out.push((
        "interrupts",
        dig(&[u8::from(s.interrupts.irq), u8::from(s.interrupts.fiq)]),
    ));

    let mp = match s.mp_state {
        vmm_backend::MpState::Runnable => 0u8,
        vmm_backend::MpState::Halted => 1,
    };
    out.push(("mp_state", dig(&[mp])));
}
