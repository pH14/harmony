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

use hypercall_proto::{Service, Status};
use vm_state::Arm64VmState;
use vmm_backend::{Arm64, Arm64VcpuState, Backend, CommonExit, Exit, Gpa};

use crate::snapshot::SnapshotError;
use crate::vendor::InterruptReject;
use crate::vendor::arm64::contract;
use crate::vendor::arm64::devices::Pl011;
use crate::vendor::arm64::records::{
    self, Arm64ClockeventState, Arm64DeviceState, Arm64PvclockState,
};
use crate::virtual_time::{DeviceClass, NormalizedEventClass};
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
// OSDLR_EL1 (S2_0_C1_C3_4). Linux writes zero during debug-monitors boot
// initialization. HVF traps that access even though it is not a GIC sysreg.
const OSDLR_EL1: u32 = 0x0028_0406;
// OSLAR_EL1 (S2_0_C1_C0_4), the companion OS lock access register.
const OSLAR_EL1: u32 = 0x0028_0400;

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

/// Convert an ARM backend exit to the substrate-independent M1 log shape.
/// Payloads are fixed-order little-endian encodings of every field that can
/// affect dispatch; no backend debug string or host address enters this log.
///
/// GIC distributor/redistributor MMIO and CPU-interface sysregs are deliberately
/// raw-only. HVF surfaces those transactions so the userspace GIC can service
/// them, while stock KVM consumes the same architectural operations inside its
/// in-kernel vGIC. Counting them would bind the portable clock and normalized
/// log to the substrate's implementation boundary.
pub(crate) fn normalize_virtual_time_exit_arm64(
    exit: &Exit<Arm64>,
) -> Option<(NormalizedEventClass, Vec<u8>)> {
    use super::board::{DOORBELL, GICD, GICR, PL011, PVCLOCK};

    match exit {
        Exit::Common(CommonExit::Mmio { gpa, size, write }) => {
            let mut payload = gpa.0.to_le_bytes().to_vec();
            payload.push(*size);
            match write {
                Some(value) => {
                    payload.push(1);
                    payload.extend_from_slice(&value.to_le_bytes());
                }
                None => payload.push(0),
            }
            let class = if in_frame(gpa.0, PL011) {
                NormalizedEventClass::DeviceMmio(DeviceClass::Serial)
            } else if in_frame(gpa.0, GICD) || in_frame(gpa.0, GICR) {
                return None;
            } else if in_frame(gpa.0, PVCLOCK) {
                NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual)
            } else if in_frame(gpa.0, DOORBELL) {
                NormalizedEventClass::Doorbell
            } else {
                // Dispatch will fail closed. Retain a stable class in the raw
                // failure trace without pretending it was a modeled device.
                NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual)
            };
            Some((class, payload))
        }
        Exit::Arch(vmm_backend::Arm64Exit::Sysreg { sysreg, write }) => {
            let _ = (sysreg, write);
            // Stock KVM services the ruled GIC CPU-interface and OS debug-lock
            // accesses without returning to userspace. HVF surfaces them only
            // because its trap surface requires userspace emulation. They are
            // substrate-private diagnostics, not portable event ordinals.
            None
        }
        Exit::Common(CommonExit::Idle) => Some((NormalizedEventClass::Idle, Vec::new())),
        Exit::Common(CommonExit::Shutdown) => Some((NormalizedEventClass::Terminal, Vec::new())),
        Exit::Common(CommonExit::Hypercall(frame)) => {
            let mut payload = Vec::new();
            for arg in frame.args {
                payload.extend_from_slice(&arg.to_le_bytes());
            }
            Some((NormalizedEventClass::Doorbell, payload))
        }
    }
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
    /// Paravirtual clockevent deadline and its virtual-timer PPI input level.
    pub(crate) clockevent: Arm64ClockeventState,
}

impl Arm64Devices {
    /// Fresh (reset) arm64 device state: a reset PL011, no fabric.
    pub(crate) fn new() -> Self {
        Self {
            uart: Pl011::new(),
            gic: None,
            clockevent: Arm64ClockeventState::default(),
        }
    }
}

impl<B: Backend<A = Arm64>> Vmm<B> {
    /// Capture live vCPU + GIC state directly for the independent M5
    /// architectural comparator. This path does not call `save_vm_state`,
    /// encode a vendor snapshot, or compute a state hash.
    pub fn arm64_architectural_state(&self) -> Result<super::Arm64ArchitecturalState, VmmError> {
        let mut vcpu = self.backend.save()?;
        let backend_gic = vcpu.gic.take().map(|gic| records::gic_from_backend(&gic));
        let userspace_gic = self.devices.gic.as_ref().map(gicv3::Gicv3::snapshot);
        let gic = match (backend_gic, userspace_gic) {
            (Some(_), Some(_)) => {
                return Err(VmmError::ContractViolation(
                    "both in-kernel and userspace GICv3 fabrics are wired".to_string(),
                ));
            }
            (Some(gic), None) | (None, Some(gic)) => Some(gic),
            (None, None) => None,
        };
        Ok(super::Arm64ArchitecturalState { vcpu, gic })
    }

    /// Read the live interrupt controller in the canonical architectural form
    /// shared by the KVM in-kernel vGIC and the HVF userspace model.
    pub fn canonical_arm64_gic_state(&self) -> Result<Option<gicv3::GicState>, VmmError> {
        let vcpu = self.backend.save()?;
        match (vcpu.gic.as_ref(), self.devices.gic.as_ref()) {
            (Some(_), Some(_)) => Err(VmmError::ContractViolation(
                "both in-kernel and userspace GICv3 fabrics are wired".to_string(),
            )),
            (Some(gic), None) => Ok(Some(records::gic_from_backend(gic))),
            (None, Some(gic)) => Ok(Some(gic.snapshot())),
            (None, None) => Ok(None),
        }
    }

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
                | OSDLR_EL1
                | OSLAR_EL1
        );
        if !ruled {
            let dir = if write.is_some() { "write" } else { "read" };
            return Err(VmmError::ContractViolation(format!(
                "trapped sysreg {dir} ({sysreg:#010x}) has no ruled disposition for HVF"
            )));
        }
        if matches!(sysreg, OSDLR_EL1 | OSLAR_EL1) {
            return match write {
                Some(0) => {
                    // The deterministic zero write only clears the OS debug
                    // lock. The retained debug register file remains the sole
                    // guest-visible debug state and already rides snapshots.
                    self.backend.complete_ok()?;
                    Ok(Step::Continued)
                }
                Some(value) => Err(VmmError::ContractViolation(format!(
                    "OS debug-lock register {sysreg:#010x} supports only Linux's deterministic \
                     zero unlock, got {value:#x}"
                ))),
                None => Err(VmmError::ContractViolation(format!(
                    "OS debug-lock register {sysreg:#010x} read has no ruled disposition"
                ))),
            };
        }
        if self.devices.gic.is_none() {
            return Err(VmmError::ContractViolation(format!(
                "trapped GIC CPU-interface sysreg {sysreg:#010x} with no userspace GIC wired"
            )));
        }
        let gic = self.devices.gic.as_mut().expect("is_none checked above");
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
                // The clockevent PPI is a level input. If a broken guest EOIs without first
                // ACKing the device, the still-high line becomes pending again.
                if intid == super::board::PVCLOCK_PPI && self.devices.clockevent.line_asserted {
                    gic.assert_line(intid).map_err(|e| {
                        VmmError::ContractViolation(format!(
                            "clockevent PPI level reassertion after EOI failed: {e}"
                        ))
                    })?;
                }
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
                gic.set_group1_enabled(value != 0);
                self.backend.complete_ok()?;
                Ok(Step::Continued)
            }
            (ICC_BPR1_EL1 | ICC_CTLR_EL1, Some(0)) => {
                self.backend.complete_ok()?;
                Ok(Step::Continued)
            }
            (ICC_IGRPEN1_EL1, None) => {
                self.backend
                    .complete_read(u64::from(gic.group1_enabled()))?;
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
        use super::board::{DOORBELL, GICD, GICR, PL011, PVCLOCK};

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
            ("pvclock", PVCLOCK),
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
                "pvclock" => matches!(size, 4 | 8),
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
            if self.virtual_time_vtime_enabled() {
                self.advance_virtual_time_vtime(contract::SERIAL_EXIT_VNS)?;
            }
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

        // The dedicated ARM pvclock + clockevent frame. Every tuple is exact:
        // offset, width, and direction are one protocol surface, not three
        // independently permissive checks.
        if in_frame(addr, PVCLOCK) {
            let offset = addr - PVCLOCK.0;
            let exact = matches!(
                (offset, size, write),
                (0x000, 8, Some(_))
                    | (0x008, 4, None)
                    | (0x010, 8, Some(_))
                    | (0x018, 4, Some(_))
                    | (0x020, 4, Some(1))
                    | (0x024, 4, Some(1))
            );
            if !exact {
                return Err(VmmError::ContractViolation(format!(
                    "arm64 pvclock MMIO protocol fault at offset {offset:#x}, size {size}, \
                     direction {}",
                    if write.is_some() { "write" } else { "read" }
                )));
            }
            if self.virtual_time_vtime_enabled() {
                let advance = if offset == 0x020 {
                    contract::EXECUTION_TICK_VNS
                } else {
                    contract::PARAVIRTUAL_EXIT_VNS
                };
                self.advance_virtual_time_vtime(advance)?;
            }
            return match (offset, write) {
                (0x000, Some(gpa)) => {
                    let (status, abi) = self.pvclock_register(gpa);
                    if status != Status::Ok || abi != Some(vtime::pvclock::PVCLOCK_ABI_VERSION) {
                        return Err(VmmError::ContractViolation(format!(
                            "arm64 pvclock registration rejected GPA {gpa:#x}: status \
                             {status:?}, abi {abi:?}"
                        )));
                    }
                    Ok(Step::Continued)
                }
                (0x008, None) => {
                    let abi = if self.pvclock_registration().is_some() {
                        vtime::pvclock::PVCLOCK_ABI_VERSION
                    } else {
                        0
                    };
                    self.backend.complete_read(u64::from(abi))?;
                    Ok(Step::Continued)
                }
                (0x010, Some(deadline)) => {
                    self.arm_clockevent_program(deadline)?;
                    Ok(Step::Continued)
                }
                (0x018, Some(control)) => {
                    let control = u32::try_from(control).map_err(|_| {
                        VmmError::ContractViolation(
                            "arm64 pvclock control value exceeds u32".to_string(),
                        )
                    })?;
                    self.arm_clockevent_control(control)?;
                    Ok(Step::Continued)
                }
                (0x020, Some(1)) => Ok(Step::Continued),
                (0x024, Some(1)) => Ok(Step::Continued),
                _ => Err(VmmError::ContractViolation(
                    "arm64 pvclock exact-shape validation disagreed with dispatch".to_string(),
                )),
            };
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
             frames, the pvclock frame, and the hypercall doorbell are modeled on the arm64 board"
        )))
    }

    /// Replace the pending absolute clockevent deadline. Programming while the
    /// clockevent-PPI device input is high is a protocol fault; the guest must ACK or
    /// DISARM first.
    fn arm_clockevent_program(&mut self, deadline: u64) -> Result<(), VmmError> {
        use super::board::PVCLOCK_PPI;

        if self.devices.clockevent.line_asserted {
            return Err(VmmError::ContractViolation(
                "arm64 clockevent deadline write while its PPI is asserted".to_string(),
            ));
        }
        self.trace_arm_clockevent_schedule(deadline, PVCLOCK_PPI)?;
        self.devices.clockevent.deadline = Some(deadline);
        Ok(())
    }

    /// Apply the exact clockevent control protocol (`1 = DISARM`, `2 = ACK`).
    fn arm_clockevent_control(&mut self, control: u32) -> Result<(), VmmError> {
        use super::board::PVCLOCK_PPI;

        match control {
            1 => {
                if self.devices.clockevent.deadline.is_some() {
                    self.trace_clockevent_cancel()?;
                }
                self.devices.clockevent.deadline = None;
                if self.devices.clockevent.line_asserted {
                    if let Some(gic) = self.devices.gic.as_mut() {
                        gic.deassert_line(PVCLOCK_PPI).map_err(|e| {
                            VmmError::ContractViolation(format!(
                                "arm64 clockevent DISARM could not lower its PPI: {e}"
                            ))
                        })?;
                    } else if self.backend.capabilities().arch.in_kernel_gic {
                        self.backend.set_pending_irq(None)?;
                    } else {
                        return Err(VmmError::ContractViolation(
                            "arm64 clockevent DISARM with no interrupt controller".to_string(),
                        ));
                    }
                    self.devices.clockevent.line_asserted = false;
                }
                Ok(())
            }
            2 if self.devices.clockevent.line_asserted => {
                if let Some(gic) = self.devices.gic.as_mut() {
                    gic.deassert_line(PVCLOCK_PPI).map_err(|e| {
                        VmmError::ContractViolation(format!(
                            "arm64 clockevent ACK could not lower its PPI: {e}"
                        ))
                    })?;
                } else if self.backend.capabilities().arch.in_kernel_gic {
                    self.backend.set_pending_irq(None)?;
                } else {
                    return Err(VmmError::ContractViolation(
                        "arm64 clockevent ACK with no interrupt controller".to_string(),
                    ));
                }
                self.devices.clockevent.line_asserted = false;
                self.devices.clockevent.acknowledgements =
                    self.devices.clockevent.acknowledgements.saturating_add(1);
                Ok(())
            }
            2 => Err(VmmError::ContractViolation(
                "arm64 clockevent ACK while its PPI is not asserted".to_string(),
            )),
            value => Err(VmmError::ContractViolation(format!(
                "arm64 clockevent control value {value} is not DISARM(1) or ACK(2)"
            ))),
        }
    }

    /// After the exit's page publication, assert the clockevent PPI iff the registered
    /// guest-clock value has reached the one pending absolute deadline.
    pub(crate) fn service_arm_clockevent_due(&mut self) -> Result<(), VmmError> {
        use super::board::PVCLOCK_PPI;

        let Some(deadline) = self.devices.clockevent.deadline else {
            return Ok(());
        };
        // Deadlines may be programmed before page registration, but the device
        // cannot evaluate or deliver them until the one-shot page is active.
        if self.pvclock_registration().is_none() {
            return Ok(());
        }
        let Some(vt) = self.vtime.as_ref() else {
            return Err(VmmError::ContractViolation(
                "arm64 clockevent deadline without V-time wiring".to_string(),
            ));
        };
        let guest_clock = vt.guest_clock();
        if guest_clock < deadline {
            return Ok(());
        }
        // A due level may only become architecturally visible at a guest-declared
        // interruptible boundary. Otherwise HVF and KVM are free to recognize the
        // already-pending IRQ at different instructions after a later DAIF unmask.
        // The cooperative guest exits immediately after every IRQ enable/restore,
        // making this PSTATE.I-clear exit the substrate-neutral delivery point.
        if self.backend.save()?.core.pstate & PSTATE_I != 0 {
            self.trace_arm_clockevent_defer()?;
            return Ok(());
        }
        if self.devices.clockevent.line_asserted {
            return Err(VmmError::ContractViolation(
                "arm64 clockevent retained a deadline while its PPI was already asserted"
                    .to_string(),
            ));
        }
        if let Some(gic) = self.devices.gic.as_mut() {
            gic.assert_line(PVCLOCK_PPI).map_err(|e| {
                VmmError::ContractViolation(format!(
                    "arm64 clockevent could not assert its PPI: {e}"
                ))
            })?;
        } else if self.backend.capabilities().arch.in_kernel_gic {
            self.backend
                .set_pending_irq(Some(vmm_backend::GicIntId(PVCLOCK_PPI)))?;
        } else {
            return Err(VmmError::ContractViolation(
                "arm64 clockevent became due with no interrupt controller".to_string(),
            ));
        }
        self.trace_clockevent_delivery()?;
        self.devices.clockevent.deadline = None;
        self.devices.clockevent.line_asserted = true;
        self.devices.clockevent.assertions = self.devices.clockevent.assertions.saturating_add(1);
        Ok(())
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
        if self.devices.gic.is_none() && !self.backend.capabilities().arch.in_kernel_gic {
            return Ok(());
        }
        if self.devices.gic.is_none() {
            let intid = self
                .devices
                .clockevent
                .line_asserted
                .then_some(vmm_backend::GicIntId(super::board::PVCLOCK_PPI));
            self.backend.set_pending_irq(intid)?;
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
        if self.devices.gic.is_none() && self.backend.capabilities().arch.in_kernel_gic {
            return Ok(self.devices.clockevent.line_asserted);
        }
        Ok(self
            .devices
            .gic
            .as_ref()
            .is_some_and(|g| g.peek_interrupt().is_some()))
    }

    /// The next armed generic-timer deadline in V-time ns (the pure
    /// deadlines-out half of the fabric). No fabric ⇒ none.
    pub(crate) fn next_timer_deadline_vns_arm64(&self) -> Option<u64> {
        let generic = self
            .devices
            .gic
            .as_ref()
            .and_then(gicv3::Gicv3::next_timer_deadline);
        let clockevent = self
            .devices
            .clockevent
            .deadline
            .and_then(|deadline| self.guest_clock_deadline_vns(deadline).ok());
        match (generic, clockevent) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (only, None) | (None, only) => only,
        }
    }

    /// [`Self::next_timer_deadline_vns_arm64`], filtered to timers whose fire
    /// would actually deliver — an armed-but-undeliverable timer is no wake.
    pub(crate) fn deliverable_timer_deadline_vns_arm64(&self) -> Option<u64> {
        let in_kernel_gic = self.backend.capabilities().arch.in_kernel_gic;
        let generic = self.devices.gic.as_ref().and_then(|gic| {
            gic.next_timer_deadline()
                .filter(|_| gic.armed_timer_deliverable())
        });
        let clockevent = self
            .devices
            .clockevent
            .deadline
            .filter(|_| self.pvclock_registration().is_some())
            .filter(|_| {
                self.devices.gic.as_ref().map_or(in_kernel_gic, |gic| {
                    gic.input_deliverable(super::board::PVCLOCK_PPI)
                })
            })
            .and_then(|deadline| self.guest_clock_deadline_vns(deadline).ok());
        match (generic, clockevent) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (only, None) | (None, only) => only,
        }
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
        gic.pulse(vector).map_err(|e| {
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
    /// V-time block anchors to the deterministic `assigned_clock`,
    /// exactly like the x86 builder.
    pub(crate) fn build_vm_state_arm64(&self, vcpu: &Arm64VcpuState) -> Arm64VmState {
        let mut s = Arm64VmState::default();
        records::fill_vcpu_state(&mut s, vcpu);
        let clock_offset = match &self.vtime {
            Some(vt) => {
                s.vtime = vm_state::VtimeState {
                    guest_hz: vt.cfg.guest_hz,
                    guest_base: vt.cfg.guest_base,
                    snapshot_vns: vt.clock.vns(),
                };
                s.hypercall = vt.entropy.save_state();
                vt.guest_clock_offset
            }
            None => {
                // Unwired: a sentinel encodable V-time block, no entropy.
                0
            }
        };
        debug_assert!(vcpu.gic.is_none() || self.devices.gic.is_none());
        let backend_gic = vcpu.gic.as_ref().map(records::gic_from_backend);
        let dev = Arm64DeviceState {
            clock_offset,
            report_stream: self.report_stream.clone(),
            uart_capture: self.devices.uart.capture().to_vec(),
            uart_regs: *self.devices.uart.shadow_regs(),
            gic: backend_gic.or_else(|| self.devices.gic.as_ref().map(gicv3::Gicv3::snapshot)),
            // The dedicated hypercall-transport ABI pages ride the blob so
            // save/restore/branch preserve them (they are a separate memslot, not
            // in the main-RAM snapshot). Empty when the VM never mapped them.
            doorbell: self
                .doorbell_pages
                .as_ref()
                .map(|db| db.as_bytes().to_vec())
                .unwrap_or_default(),
            pvclock: self.pvclock_snapshot().map(|pv| Arm64PvclockState {
                gpa: pv.gpa,
                registrable: pv.registrable,
                virtual_time: self.virtual_time_vtime_enabled(),
                clockevent: self.devices.clockevent,
            }),
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
        let in_kernel_gic = self.backend.capabilities().arch.in_kernel_gic;
        let new_gic = match (&dev.gic, in_kernel_gic, self.devices.gic.as_ref()) {
            (Some(gs), false, Some(target)) => {
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
            (Some(gs), true, None) => {
                let have = super::board::gic_config();
                if (gs.impl_spis, gs.timer_hz, gs.timer_intid)
                    != (have.impl_spis, have.timer_hz, have.timer_intid)
                {
                    return Err(VmmError::ContractViolation(format!(
                        "restore_vm_state: GICv3 config mismatch (snapshot impl_spis={} timer_hz={} \
                         timer_intid={} vs this VM's {}/{}/{}) — restore into a VM composed like \
                         the snapshot source.",
                        gs.impl_spis,
                        gs.timer_hz,
                        gs.timer_intid,
                        have.impl_spis,
                        have.timer_hz,
                        have.timer_intid
                    )));
                }
                // Run the same independent userspace-model validator over the
                // canonical record even though KVM will own the restored fabric.
                let _ = gicv3::Gicv3::restore(gs, s.vtime.snapshot_vns).map_err(|_| {
                    SnapshotError::DeviceRestore("incoherent GicState in device blob")
                })?;
                None
            }
            (Some(_), true, Some(_)) => {
                return Err(VmmError::ContractViolation(
                    "restore_vm_state: target composes both in-kernel and userspace GICv3 fabrics"
                        .to_string(),
                ));
            }
            (Some(_), false, None) | (None, _, Some(_)) | (None, true, None) => {
                return Err(VmmError::ContractViolation(
                    "restore_vm_state: snapshot/VM GICv3 wiring mismatch (one has the fabric, \
                     the other does not) — restore into a VM composed like the snapshot source."
                        .to_string(),
                ));
            }
            (None, false, None) => None,
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
        let pvclock_record = dev.pvclock.map(|pv| (pv.gpa, pv.registrable));
        self.pvclock_validate_restore(pvclock_record.as_ref())?;
        if let Some(pv) = dev.pvclock {
            if pv.virtual_time != self.virtual_time_vtime_enabled() {
                return Err(VmmError::ContractViolation(
                    "restore_vm_state: ARM V-time mode mismatch (snapshot and target disagree on \
                     assigned-at-exit/virtual_time mode) — restore into a VM composed like the \
                     snapshot source."
                        .to_string(),
                ));
            }
            if pv.clockevent.acknowledgements > pv.clockevent.assertions {
                return Err(VmmError::Snapshot(SnapshotError::DeviceRestore(
                    "clockevent ACK count exceeds assertion count",
                )));
            }
            if pv.clockevent.line_asserted {
                if pv.gpa.is_none() {
                    return Err(VmmError::Snapshot(SnapshotError::DeviceRestore(
                        "asserted clockevent line without a registered pvclock page",
                    )));
                }
                let Some(gs) = dev.gic.as_ref() else {
                    return Err(VmmError::Snapshot(SnapshotError::DeviceRestore(
                        "asserted clockevent line without a GIC record",
                    )));
                };
                let mask = 1u32 << super::board::PVCLOCK_PPI;
                if (gs.pending[0] | gs.active[0] | gs.line_level[0]) & mask == 0 {
                    return Err(VmmError::Snapshot(SnapshotError::DeviceRestore(
                        "asserted clockevent line absent from GIC pending/active state",
                    )));
                }
            }
        }
        let mut vcpu = records::vcpu_state_from(s);
        if in_kernel_gic {
            vcpu.gic = dev.gic.as_ref().map(records::gic_to_backend);
        }
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
        let pvclock_record = dev.pvclock.map(|pv| (pv.gpa, pv.registrable));
        self.devices.clockevent = dev
            .pvclock
            .map_or_else(Arm64ClockeventState::default, |pv| pv.clockevent);
        self.pvclock_commit_restore(pvclock_record.as_ref());
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
pub(crate) fn device_components(
    vcpu: &Arm64VcpuState,
    devices: &Arm64Devices,
    out: &mut Vec<(&'static str, [u8; 32])>,
) {
    let backend_gic = vcpu.gic.as_ref().map(records::gic_from_backend);
    let userspace_gic = devices.gic.as_ref().map(gicv3::Gicv3::snapshot);
    if let Some(gic) = backend_gic.as_ref().or(userspace_gic.as_ref()) {
        let mut bytes = Vec::new();
        records::encode_gic_state(&mut bytes, gic);
        out.push(("gic", dig(&bytes)));
    }
    if devices.clockevent != Arm64ClockeventState::default() {
        let mut bytes = Vec::new();
        records::encode_clockevent_state(&mut bytes, devices.clockevent);
        out.push(("arm-clockevent", dig(&bytes)));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vendor::arm64::board::{GICD, GICR, PL011};

    #[test]
    fn substrate_private_gic_transactions_are_raw_only() {
        for gpa in [GICD.0, GICR.0] {
            let exit = Exit::Common(CommonExit::Mmio {
                gpa: Gpa(gpa),
                size: 4,
                write: None,
            });
            assert_eq!(normalize_virtual_time_exit_arm64(&exit), None);
        }

        let cpu_interface = Exit::Arch(vmm_backend::Arm64Exit::Sysreg {
            sysreg: ICC_IAR1_EL1,
            write: None,
        });
        assert_eq!(normalize_virtual_time_exit_arm64(&cpu_interface), None);

        for sysreg in [OSDLR_EL1, OSLAR_EL1] {
            let debug_unlock = Exit::Arch(vmm_backend::Arm64Exit::Sysreg {
                sysreg,
                write: Some(0),
            });
            assert_eq!(normalize_virtual_time_exit_arm64(&debug_unlock), None);

            // Planted negative: treating an HVF-only trap as a portable event
            // would consume an ordinal and diverge from stock KVM.
            let leaked = Some((
                NormalizedEventClass::ArchitecturalControl,
                sysreg.to_le_bytes().to_vec(),
            ));
            assert_ne!(normalize_virtual_time_exit_arm64(&debug_unlock), leaked);
        }

        let serial = Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PL011.0),
            size: 4,
            write: None,
        });
        assert!(normalize_virtual_time_exit_arm64(&serial).is_some());
    }
}
