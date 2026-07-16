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
//! only on Altra-spike GO. The interrupt fabric is unwired until the `gicv3`
//! model lands (M2) and **delivery** into a real guest is `TODO(AA-6)` (the
//! vGICv3 round-trip verdict); the boot path lands with M3; the KVM backend
//! with M4. Nothing here claims silicon behavior.

pub mod contract;
pub mod devices;
pub mod dispatch;
pub mod records;

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

    fn hash_device_chunks(devices: &Self::Devices, out: &mut Vec<u8>) {
        // The GICv3 chunk is present **only** when the fabric is wired;
        // unwired compositions emit none, so their hash is byte-for-byte
        // unchanged (the x86 LAPC discipline). It captures the register files
        // + timer bookkeeping that govern future interrupt delivery.
        if let Some(gic) = &devices.gic {
            let mut bytes = Vec::new();
            records::encode_gic_state(&mut bytes, &gic.snapshot());
            crate::vmm::put_chunk(out, b"GICV", &bytes);
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

    fn vcpu_has_inflight_injection(vcpu: &Arm64VcpuState) -> bool {
        // The skeleton record set carries no pending-event records (the arm64
        // `KVM_GET_VCPU_EVENTS` surface — pending SError — is part of the
        // AA-6 record-set decision), so nothing representable is in flight.
        let _ = vcpu;
        false
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
