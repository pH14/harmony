// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **arm64 vendor** (`docs/ARCH-BOUNDARY.md` §B/§D, `tasks/112`):
//! everything in the deterministic VMM that names the arm64 ISA — the
//! CPU-contract policy skeleton ([`contract`]), the exit dispatch and the
//! device models ([`dispatch`], [`devices`]), and the `vm_state` record set
//! glue ([`records`]).
//!
//! The engine ([`crate::vmm`]) reaches all of it through [`Vendor`] alone —
//! this module is the **first real second implementor**, the structural check
//! that the seam is genuinely additive (a signature only a second vendor could
//! refute stays invisible until one instantiates the trait).
//!
//! **A skeleton, deliberately** (the §Pre-build ruling): built against the
//! unfrozen trait (designed-not-frozen — AA-3's memo owns the freeze), trusted
//! only after M4's native msr1 validation. The interrupt fabric is unwired until
//! the `gicv3`
//! model lands (M2) and **delivery** into a real guest is `TODO(AA-6)` (the
//! vGICv3 round-trip verdict); the boot path lands with M3; the KVM backend
//! with M4. Nothing here claims silicon behavior.

use std::io::{self, Write};

pub mod board;
pub mod bringup;
pub mod contract;
pub mod devices;
pub mod dispatch;
pub mod dtb;
pub mod entry;
pub mod hostassert;
pub mod image_loader;
pub mod records;

/// First field-level disagreement reported by the independent architectural
/// GIC comparator. This comparator does not consume the snapshot encoding or
/// its hash; it compares the typed architectural record directly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GicArchitectureDifference {
    /// Stable field name.
    pub field: &'static str,
    /// Element index for an array field.
    pub index: Option<usize>,
}

/// Compare two canonical GICv3 records field by field, independently of the
/// state-hash and device-blob codecs.
pub fn compare_gic_architecture(
    expected: &gicv3::GicState,
    actual: &gicv3::GicState,
) -> Result<(), GicArchitectureDifference> {
    macro_rules! scalar {
        ($field:ident) => {
            if expected.$field != actual.$field {
                return Err(GicArchitectureDifference {
                    field: stringify!($field),
                    index: None,
                });
            }
        };
    }
    scalar!(version);
    scalar!(impl_spis);
    scalar!(timer_hz);
    scalar!(timer_intid);
    scalar!(gicd_ctlr);
    scalar!(pmr);
    scalar!(igrpen1);
    scalar!(cntv_ctl);
    scalar!(cntv_cval);
    scalar!(timer_fired);
    macro_rules! array {
        ($field:ident) => {
            if let Some(index) = expected
                .$field
                .iter()
                .zip(actual.$field.iter())
                .position(|(a, b)| a != b)
            {
                return Err(GicArchitectureDifference {
                    field: stringify!($field),
                    index: Some(index),
                });
            }
        };
    }
    array!(group);
    array!(enable);
    array!(pending);
    array!(active);
    array!(line_level);
    array!(priority);
    Ok(())
}

/// Direct, substrate-neutral architectural capture used by M5's comparator.
///
/// The vCPU record comes straight from the backend's live save seam. Any
/// backend-owned in-kernel GIC is removed from that record and normalized into
/// `gic`, where it has the same typed form as the userspace HVF model. This is
/// deliberately separate from `state_blob`, the vendor snapshot codec, and
/// `state_hash`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Arm64ArchitecturalState {
    /// Canonical live vCPU state, with `gic == None` by construction.
    pub vcpu: vmm_backend::Arm64VcpuState,
    /// Canonical GICv3 architectural record, independent of fabric ownership.
    pub gic: Option<gicv3::GicState>,
}

impl Arm64ArchitecturalState {
    /// Write a stable, field-explicit architectural evidence record.
    ///
    /// This format is deliberately independent of the vendor snapshot codec
    /// and `state_hash`. Every field consumed by [`compare_arm64_architecture`]
    /// is emitted in comparator order, so ordinary byte comparison of records
    /// captured on two hosts is a second implementation of the typed oracle.
    pub fn write_text(&self, mut out: impl Write) -> io::Result<()> {
        if self.vcpu.gic.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "architectural capture retains a non-canonical embedded GIC",
            ));
        }
        writeln!(out, "format consonance.arm64-architecture.v1")?;
        for (index, value) in self.vcpu.core.x.iter().enumerate() {
            writeln!(out, "core.x.{index} {value:016x}")?;
        }
        write_u64(&mut out, "core.sp", self.vcpu.core.sp)?;
        write_u64(&mut out, "core.pc", self.vcpu.core.pc)?;
        write_u64(&mut out, "core.pstate", self.vcpu.core.pstate)?;
        write_u64(&mut out, "core.sp_el1", self.vcpu.core.sp_el1)?;
        write_u64(&mut out, "core.elr_el1", self.vcpu.core.elr_el1)?;
        write_u64(&mut out, "core.spsr_el1", self.vcpu.core.spsr_el1)?;

        write_u64(&mut out, "sysregs.sctlr_el1", self.vcpu.sysregs.sctlr_el1)?;
        write_u64(&mut out, "sysregs.ttbr0_el1", self.vcpu.sysregs.ttbr0_el1)?;
        write_u64(&mut out, "sysregs.ttbr1_el1", self.vcpu.sysregs.ttbr1_el1)?;
        write_u64(&mut out, "sysregs.tcr_el1", self.vcpu.sysregs.tcr_el1)?;
        write_u64(&mut out, "sysregs.mair_el1", self.vcpu.sysregs.mair_el1)?;
        write_u64(&mut out, "sysregs.vbar_el1", self.vcpu.sysregs.vbar_el1)?;
        write_u64(&mut out, "sysregs.cpacr_el1", self.vcpu.sysregs.cpacr_el1)?;
        write_u64(&mut out, "sysregs.esr_el1", self.vcpu.sysregs.esr_el1)?;
        write_u64(&mut out, "sysregs.far_el1", self.vcpu.sysregs.far_el1)?;
        write_u64(&mut out, "sysregs.tpidr_el0", self.vcpu.sysregs.tpidr_el0)?;
        write_u64(&mut out, "sysregs.tpidr_el1", self.vcpu.sysregs.tpidr_el1)?;
        write_u64(
            &mut out,
            "sysregs.cntkctl_el1",
            self.vcpu.sysregs.cntkctl_el1,
        )?;

        for (index, value) in self.vcpu.simd_fp.q.iter().enumerate() {
            write!(out, "simd_fp.q.{index} ")?;
            write_hex(&mut out, value)?;
            writeln!(out)?;
        }
        write_u64(&mut out, "simd_fp.fpcr", self.vcpu.simd_fp.fpcr)?;
        write_u64(&mut out, "simd_fp.fpsr", self.vcpu.simd_fp.fpsr)?;

        write_u64_array(
            &mut out,
            "debug.breakpoint_value",
            &self.vcpu.debug.breakpoint_value,
        )?;
        write_u64_array(
            &mut out,
            "debug.breakpoint_control",
            &self.vcpu.debug.breakpoint_control,
        )?;
        write_u64_array(
            &mut out,
            "debug.watchpoint_value",
            &self.vcpu.debug.watchpoint_value,
        )?;
        write_u64_array(
            &mut out,
            "debug.watchpoint_control",
            &self.vcpu.debug.watchpoint_control,
        )?;
        write_u64(&mut out, "debug.mdscr_el1", self.vcpu.debug.mdscr_el1)?;
        write_bool(
            &mut out,
            "debug.trap_debug_exceptions",
            self.vcpu.debug.trap_debug_exceptions,
        )?;
        write_bool(
            &mut out,
            "debug.trap_debug_reg_accesses",
            self.vcpu.debug.trap_debug_reg_accesses,
        )?;

        write_u64(
            &mut out,
            "vtimer.cntv_ctl_el0",
            self.vcpu.vtimer.cntv_ctl_el0,
        )?;
        write_u64(
            &mut out,
            "vtimer.cntv_cval_el0",
            self.vcpu.vtimer.cntv_cval_el0,
        )?;
        write_bool(&mut out, "vtimer.masked", self.vcpu.vtimer.masked)?;
        write_u64(&mut out, "vtimer.offset", self.vcpu.vtimer.offset)?;
        write_bool(&mut out, "interrupts.irq", self.vcpu.interrupts.irq)?;
        write_bool(&mut out, "interrupts.fiq", self.vcpu.interrupts.fiq)?;
        writeln!(
            out,
            "mp_state {}",
            match self.vcpu.mp_state {
                vmm_backend::MpState::Runnable => 0,
                vmm_backend::MpState::Halted => 1,
            }
        )?;

        let Some(gic) = &self.gic else {
            writeln!(out, "gic none")?;
            return Ok(());
        };
        writeln!(out, "gic present")?;
        writeln!(out, "gic.version {:08x}", gic.version)?;
        writeln!(out, "gic.impl_spis {:08x}", gic.impl_spis)?;
        write_u64(&mut out, "gic.timer_hz", gic.timer_hz)?;
        writeln!(out, "gic.timer_intid {:08x}", gic.timer_intid)?;
        writeln!(out, "gic.gicd_ctlr {:08x}", gic.gicd_ctlr)?;
        write_u32_array(&mut out, "gic.group", &gic.group)?;
        write_u32_array(&mut out, "gic.enable", &gic.enable)?;
        write_u32_array(&mut out, "gic.pending", &gic.pending)?;
        write_u32_array(&mut out, "gic.active", &gic.active)?;
        write_u32_array(&mut out, "gic.line_level", &gic.line_level)?;
        for (index, value) in gic.priority.iter().enumerate() {
            writeln!(out, "gic.priority.{index} {value:02x}")?;
        }
        writeln!(out, "gic.pmr {:02x}", gic.pmr)?;
        write_bool(&mut out, "gic.igrpen1", gic.igrpen1)?;
        write_u64(&mut out, "gic.cntv_ctl", gic.cntv_ctl)?;
        write_u64(&mut out, "gic.cntv_cval", gic.cntv_cval)?;
        write_bool(&mut out, "gic.timer_fired", gic.timer_fired)
    }
}

fn write_u64(out: &mut impl Write, name: &str, value: u64) -> io::Result<()> {
    writeln!(out, "{name} {value:016x}")
}

fn write_bool(out: &mut impl Write, name: &str, value: bool) -> io::Result<()> {
    writeln!(out, "{name} {}", u8::from(value))
}

fn write_u64_array(out: &mut impl Write, name: &str, values: &[u64]) -> io::Result<()> {
    for (index, value) in values.iter().enumerate() {
        writeln!(out, "{name}.{index} {value:016x}")?;
    }
    Ok(())
}

fn write_u32_array(out: &mut impl Write, name: &str, values: &[u32]) -> io::Result<()> {
    for (index, value) in values.iter().enumerate() {
        writeln!(out, "{name}.{index} {value:08x}")?;
    }
    Ok(())
}

fn write_hex(out: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    for byte in bytes {
        write!(out, "{byte:02x}")?;
    }
    Ok(())
}

/// First field-level disagreement from the independent ARM comparator.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arm64ArchitectureDifference {
    /// A scalar or indexed vCPU register file differs.
    Vcpu {
        /// Stable architectural field name.
        field: &'static str,
        /// Array element, when the field is an architectural register bank.
        index: Option<usize>,
    },
    /// One capture has an architectural GIC and the other does not.
    GicPresence,
    /// Both captures have a GIC and the independent GIC comparator localized it.
    Gic(GicArchitectureDifference),
}

/// Compare two direct ARM architectural captures field by field.
///
/// This does not consume a state hash, a component digest, or the vendor
/// snapshot encoding. It is therefore an independent comparator for the M5
/// portability result rather than a second spelling of the canonical hash.
pub fn compare_arm64_architecture(
    expected: &Arm64ArchitecturalState,
    actual: &Arm64ArchitecturalState,
) -> Result<(), Arm64ArchitectureDifference> {
    let a = &expected.vcpu;
    let b = &actual.vcpu;
    macro_rules! scalar {
        ($field:literal, $a:expr, $b:expr) => {
            if $a != $b {
                return Err(Arm64ArchitectureDifference::Vcpu {
                    field: $field,
                    index: None,
                });
            }
        };
    }
    macro_rules! array {
        ($field:literal, $a:expr, $b:expr) => {
            if let Some(index) = $a.iter().zip($b.iter()).position(|(x, y)| x != y) {
                return Err(Arm64ArchitectureDifference::Vcpu {
                    field: $field,
                    index: Some(index),
                });
            }
        };
    }

    array!("core.x", a.core.x, b.core.x);
    scalar!("core.sp", a.core.sp, b.core.sp);
    scalar!("core.pc", a.core.pc, b.core.pc);
    scalar!("core.pstate", a.core.pstate, b.core.pstate);
    scalar!("core.sp_el1", a.core.sp_el1, b.core.sp_el1);
    scalar!("core.elr_el1", a.core.elr_el1, b.core.elr_el1);
    scalar!("core.spsr_el1", a.core.spsr_el1, b.core.spsr_el1);

    scalar!(
        "sysregs.sctlr_el1",
        a.sysregs.sctlr_el1,
        b.sysregs.sctlr_el1
    );
    scalar!(
        "sysregs.ttbr0_el1",
        a.sysregs.ttbr0_el1,
        b.sysregs.ttbr0_el1
    );
    scalar!(
        "sysregs.ttbr1_el1",
        a.sysregs.ttbr1_el1,
        b.sysregs.ttbr1_el1
    );
    scalar!("sysregs.tcr_el1", a.sysregs.tcr_el1, b.sysregs.tcr_el1);
    scalar!("sysregs.mair_el1", a.sysregs.mair_el1, b.sysregs.mair_el1);
    scalar!("sysregs.vbar_el1", a.sysregs.vbar_el1, b.sysregs.vbar_el1);
    scalar!(
        "sysregs.cpacr_el1",
        a.sysregs.cpacr_el1,
        b.sysregs.cpacr_el1
    );
    scalar!("sysregs.esr_el1", a.sysregs.esr_el1, b.sysregs.esr_el1);
    scalar!("sysregs.far_el1", a.sysregs.far_el1, b.sysregs.far_el1);
    scalar!(
        "sysregs.tpidr_el0",
        a.sysregs.tpidr_el0,
        b.sysregs.tpidr_el0
    );
    scalar!(
        "sysregs.tpidr_el1",
        a.sysregs.tpidr_el1,
        b.sysregs.tpidr_el1
    );
    scalar!(
        "sysregs.cntkctl_el1",
        a.sysregs.cntkctl_el1,
        b.sysregs.cntkctl_el1
    );

    array!("simd_fp.q", a.simd_fp.q, b.simd_fp.q);
    scalar!("simd_fp.fpcr", a.simd_fp.fpcr, b.simd_fp.fpcr);
    scalar!("simd_fp.fpsr", a.simd_fp.fpsr, b.simd_fp.fpsr);
    array!(
        "debug.breakpoint_value",
        a.debug.breakpoint_value,
        b.debug.breakpoint_value
    );
    array!(
        "debug.breakpoint_control",
        a.debug.breakpoint_control,
        b.debug.breakpoint_control
    );
    array!(
        "debug.watchpoint_value",
        a.debug.watchpoint_value,
        b.debug.watchpoint_value
    );
    array!(
        "debug.watchpoint_control",
        a.debug.watchpoint_control,
        b.debug.watchpoint_control
    );
    scalar!("debug.mdscr_el1", a.debug.mdscr_el1, b.debug.mdscr_el1);
    scalar!(
        "debug.trap_debug_exceptions",
        a.debug.trap_debug_exceptions,
        b.debug.trap_debug_exceptions
    );
    scalar!(
        "debug.trap_debug_reg_accesses",
        a.debug.trap_debug_reg_accesses,
        b.debug.trap_debug_reg_accesses
    );
    scalar!(
        "vtimer.cntv_ctl_el0",
        a.vtimer.cntv_ctl_el0,
        b.vtimer.cntv_ctl_el0
    );
    scalar!(
        "vtimer.cntv_cval_el0",
        a.vtimer.cntv_cval_el0,
        b.vtimer.cntv_cval_el0
    );
    scalar!("vtimer.masked", a.vtimer.masked, b.vtimer.masked);
    scalar!("vtimer.offset", a.vtimer.offset, b.vtimer.offset);
    scalar!("interrupts.irq", a.interrupts.irq, b.interrupts.irq);
    scalar!("interrupts.fiq", a.interrupts.fiq, b.interrupts.fiq);
    scalar!("mp_state", a.mp_state, b.mp_state);
    if a.gic.is_some() || b.gic.is_some() {
        return Err(Arm64ArchitectureDifference::Vcpu {
            field: "vcpu.gic",
            index: None,
        });
    }

    match (&expected.gic, &actual.gic) {
        (None, None) => Ok(()),
        (Some(expected), Some(actual)) => {
            compare_gic_architecture(expected, actual).map_err(Arm64ArchitectureDifference::Gic)
        }
        _ => Err(Arm64ArchitectureDifference::GicPresence),
    }
}

use control_proto::RegsView;
use vm_state::Arm64VmState;
use vmm_backend::{Arm64, Arm64Exit, Arm64VcpuState, Backend, Gpa};

pub use dispatch::Arm64Devices;

use crate::vendor::{InterruptReject, Vendor};
use crate::vmm::{Step, Vmm, VmmError};

impl Vendor for Arm64 {
    type Devices = Arm64Devices;
    type RestorePrep = dispatch::Arm64RestorePrep;
    type Snapshot = Arm64VmState;

    fn new_devices() -> Self::Devices {
        Arm64Devices::new()
    }

    fn mmio_holes() -> &'static [(u64, u64)] {
        // No machine memory map exists yet — the arm64 board layout (GIC
        // frames, PL011, the reserved doorbell GPA) lands with the M3 boot
        // path, and until then the skeleton punches no holes: every MMIO
        // access fails closed in `dispatch_mmio` regardless.
        &[]
    }

    fn dispatch_arch<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        exit: Arm64Exit,
    ) -> Result<Step, VmmError> {
        // Exhaustive over `Arm64Exit` — no wildcard arm (default-deny stays
        // structural; `docs/ARCH-BOUNDARY.md` §A).
        match exit {
            Arm64Exit::Sysreg { sysreg, write } => vmm.dispatch_sysreg(sysreg, write),
        }
    }

    fn dispatch_mmio<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        gpa: Gpa,
        size: u8,
        write: Option<u64>,
    ) -> Result<Step, VmmError> {
        vmm.dispatch_mmio_arm64(gpa, size, write)
    }

    fn post_exit<B: Backend<A = Self>>(vmm: &mut Vmm<B>) -> Result<(), VmmError> {
        vmm.service_arm_clockevent_due()
    }

    fn normalize_prescriptive_exit(
        exit: &vmm_backend::Exit<Self>,
    ) -> Option<(crate::prescriptive::NormalizedEventClass, Vec<u8>)> {
        dispatch::normalize_prescriptive_exit_arm64(exit)
    }

    fn service_pending_irqs<B: Backend<A = Self>>(vmm: &mut Vmm<B>) -> Result<(), VmmError> {
        vmm.service_pending_irqs_arm64()
    }

    fn complete_irq_delivery<B: Backend<A = Self>>(vmm: &mut Vmm<B>) {
        vmm.complete_irq_delivery_arm64();
    }

    fn guest_interruptible<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Result<bool, VmmError> {
        // `PSTATE.I` clear — the guest's own "I can take an IRQ" signal (the
        // arm64 mirror of x86's `RFLAGS.IF`; `PSTATE.F`/FIQ is not modeled by
        // the skeleton — TODO(AA-6): the contract's group model).
        Ok(vmm.backend().save()?.core.pstate & dispatch::PSTATE_I == 0)
    }

    fn pending_deliverable_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
    ) -> Result<bool, VmmError> {
        vmm.pending_deliverable_interrupt_arm64()
    }

    fn next_timer_deadline_vns<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Option<u64> {
        vmm.next_timer_deadline_vns_arm64()
    }

    fn deliverable_timer_deadline_vns<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Option<u64> {
        vmm.deliverable_timer_deadline_vns_arm64()
    }

    fn check_wire_interrupt<B: Backend<A = Self>>(
        vmm: &Vmm<B>,
        vector: u32,
    ) -> Result<(), InterruptReject> {
        vmm.check_wire_interrupt_arm64(vector)
    }

    fn inject_wire_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        vector: u32,
    ) -> Result<(), VmmError> {
        vmm.inject_host_interrupt_arm64(vector)
    }

    fn has_pending_guest_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
    ) -> Result<bool, VmmError> {
        vmm.has_pending_guest_interrupt_arm64()
    }

    fn serial_capture(devices: &Self::Devices) -> &[u8] {
        devices.uart.capture()
    }

    fn inject_serial_input(devices: &mut Self::Devices, bytes: &[u8]) {
        devices.uart.inject_input(bytes);
    }

    fn encode_vcpu_chunk(vcpu: &Arm64VcpuState) -> Vec<u8> {
        dispatch::encode_vcpu_state(vcpu)
    }

    fn encode_device_state(devices: &Self::Devices) -> Vec<u8> {
        // The PL011 configuration-register shadows — the device's residual
        // state, so two runs that program the UART differently hash
        // differently even with byte-identical serial output. (The engine
        // appends its terminal-reason bytes after this.)
        let mut v = Vec::new();
        for r in devices.uart.shadow_regs() {
            v.extend_from_slice(&r.to_le_bytes());
        }
        v
    }

    fn hash_device_chunks(vcpu: &Arm64VcpuState, devices: &Self::Devices, out: &mut Vec<u8>) {
        // The GICv3 chunk is present **only** when the fabric is wired;
        // unwired compositions emit none, so their hash is byte-for-byte
        // unchanged (the x86 LAPC discipline). It captures the register files
        // + timer bookkeeping that govern future interrupt delivery.
        let backend_gic = vcpu.gic.as_ref().map(records::gic_from_backend);
        let userspace_gic = devices.gic.as_ref().map(gicv3::Gicv3::snapshot);
        let gic = backend_gic.as_ref().or(userspace_gic.as_ref());
        if let Some(gic) = gic {
            let mut bytes = Vec::new();
            records::encode_gic_state(&mut bytes, gic);
            crate::vmm::put_chunk(out, b"GICV", &bytes);
        }
        if devices.clockevent != records::Arm64ClockeventState::default() {
            let mut bytes = Vec::new();
            records::encode_clockevent_state(&mut bytes, devices.clockevent);
            crate::vmm::put_chunk(out, b"PVCE", &bytes);
        }
    }

    fn regs_view(vcpu: &Arm64VcpuState) -> RegsView {
        // The task-80 wire view is x86-shaped (v1); fill the arm64 core subset
        // into its canonical slots — `x0..x15` in the GPR array, `PC` as the
        // instruction pointer, `PSTATE` as the flags word — and leave the
        // segment/control-register slots zero (arm64 has none of them; a full
        // arm64 view is an additive schema bump, port work — the view's
        // `version` field exists for exactly that evolution).
        let mut gpr = [0u64; 16];
        gpr.copy_from_slice(&vcpu.core.x[..16]);
        RegsView {
            version: RegsView::VERSION,
            gpr,
            rip: vcpu.core.pc,
            rflags: vcpu.core.pstate,
            seg: [0; 6],
            cr0: 0,
            cr3: 0,
            cr4: 0,
            moment: control_proto::Moment(0),
            vtime: 0,
        }
    }

    fn vcpu_components(vcpu: &Arm64VcpuState, out: &mut Vec<(&'static str, [u8; 32])>) {
        dispatch::vcpu_components(vcpu, out);
    }

    fn device_components(
        vcpu: &Arm64VcpuState,
        devices: &Self::Devices,
        out: &mut Vec<(&'static str, [u8; 32])>,
    ) {
        // Expose the GICv3 to the diagnostic breakdown when the fabric is wired,
        // digesting **exactly the bytes the `GICV` hash chunk hashes** (see
        // [`hash_device_chunks`]) — so a `state_hash` divergence that lives only
        // in the GIC (register files / pending-active / the virtual timer)
        // localizes to the `gic` component instead of "diverged but every
        // component matched". A new label (never a rename); unwired ⇒ nothing.
        dispatch::device_components(vcpu, devices, out);
    }

    fn vcpu_has_inflight_injection(vcpu: &Arm64VcpuState) -> bool {
        vcpu.interrupts.irq || vcpu.interrupts.fiq
    }

    fn vcpu_has_active_injection(vcpu: &Arm64VcpuState) -> bool {
        let _ = vcpu;
        false
    }

    fn check_sealable_vcpu(vcpu: &Arm64VcpuState) -> Result<(), VmmError> {
        // Every field of the skeleton vCPU record is representable in the
        // skeleton record set by construction (they mirror one another
        // field-for-field). The real unrepresentability check — which live
        // machine state the sealed subset would silently drop — arrives with
        // the AA-6 record set, alongside the state itself.
        let _ = vcpu;
        Ok(())
    }

    fn build_vm_state<B: Backend<A = Self>>(vmm: &Vmm<B>, vcpu: &Arm64VcpuState) -> Arm64VmState {
        vmm.build_vm_state_arm64(vcpu)
    }

    fn validate_restore<B: Backend<A = Self>>(
        vmm: &Vmm<B>,
        s: &Arm64VmState,
    ) -> Result<(Arm64VcpuState, u64, Self::RestorePrep), VmmError> {
        vmm.validate_restore_arm64(s)
    }

    fn commit_restore<B: Backend<A = Self>>(vmm: &mut Vmm<B>, prep: Self::RestorePrep) {
        vmm.commit_restore_arm64(prep);
    }
}

#[cfg(test)]
mod comparator_tests {
    use super::*;

    #[test]
    fn architectural_comparator_localizes_a_planted_gic_corruption() {
        let expected = board::new_gic().snapshot();
        let mut planted = expected.clone();
        planted.priority[27] ^= 1;
        assert_eq!(
            compare_gic_architecture(&expected, &planted),
            Err(GicArchitectureDifference {
                field: "priority",
                index: Some(27),
            })
        );
        let mut planted = expected.clone();
        planted.gicd_ctlr ^= 1 << 1;
        assert_eq!(
            compare_gic_architecture(&expected, &planted),
            Err(GicArchitectureDifference {
                field: "gicd_ctlr",
                index: None,
            })
        );
        assert_eq!(compare_gic_architecture(&expected, &expected), Ok(()));
    }

    #[test]
    fn full_architectural_comparator_localizes_a_planted_core_register() {
        let expected = Arm64ArchitecturalState {
            vcpu: vmm_backend::Arm64VcpuState::default(),
            gic: Some(board::new_gic().snapshot()),
        };
        let mut expected_text = Vec::new();
        expected.write_text(&mut expected_text).unwrap();
        assert!(expected_text.starts_with(b"format consonance.arm64-architecture.v1\n"));
        let mut planted = expected.clone();
        planted.vcpu.core.x[7] = 1;
        let mut planted_text = Vec::new();
        planted.write_text(&mut planted_text).unwrap();
        assert_ne!(expected_text, planted_text);
        assert_eq!(
            compare_arm64_architecture(&expected, &planted),
            Err(Arm64ArchitectureDifference::Vcpu {
                field: "core.x",
                index: Some(7),
            })
        );
        assert_eq!(compare_arm64_architecture(&expected, &expected), Ok(()));
        let mut repeated_text = Vec::new();
        expected.write_text(&mut repeated_text).unwrap();
        assert_eq!(expected_text, repeated_text);
    }

    #[test]
    fn architectural_capture_rejects_an_embedded_gic_as_noncanonical() {
        let gic = board::new_gic().snapshot();
        let embedded = Arm64ArchitecturalState {
            vcpu: vmm_backend::Arm64VcpuState {
                gic: Some(records::gic_to_backend(&gic)),
                ..Default::default()
            },
            gic: Some(gic),
        };
        assert_eq!(
            compare_arm64_architecture(&embedded, &embedded),
            Err(Arm64ArchitectureDifference::Vcpu {
                field: "vcpu.gic",
                index: None,
            })
        );
        assert_eq!(
            embedded.write_text(Vec::new()).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
