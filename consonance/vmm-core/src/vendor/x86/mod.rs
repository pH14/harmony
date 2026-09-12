// SPDX-License-Identifier: AGPL-3.0-or-later

pub mod bringup;
pub mod contract;
pub mod devices;
pub mod dispatch;
pub mod entry;
pub mod linux_loader;
pub mod records;

use control_proto::RegsView;
use vm_state::VmState;
use vmm_backend::{Backend, Exit, Gpa, X86, X86Exit};

pub use dispatch::{X86Devices, contract_vclock_config};

use crate::vendor::x86::linux_loader::{LAPIC_MMIO_PAGE, LAPIC_MMIO_PAGE_LEN};
use crate::vendor::{InterruptReject, Vendor};
use crate::vmm::{Step, Vmm, VmmError};

impl Vendor for X86 {
    type Devices = X86Devices;
    type RestorePrep = dispatch::X86RestorePrep;
    type Snapshot = VmState;

    fn new_devices() -> Self::Devices {
        X86Devices::new()
    }

    fn mmio_holes() -> &'static [(u64, u64)] {
        &[(LAPIC_MMIO_PAGE, LAPIC_MMIO_PAGE_LEN)]
    }

    fn dispatch_arch<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        exit: X86Exit,
    ) -> Result<Step, VmmError> {
        match exit {
            X86Exit::Io {
                port,
                size,
                write: Some(v),
            } => vmm.dispatch_out(port, size, v),
            X86Exit::Io {
                port,
                size,
                write: None,
            } => vmm.dispatch_in(port, size),
            X86Exit::Rdmsr { index } => vmm.dispatch_rdmsr(index),
            X86Exit::Wrmsr { index, value } => vmm.dispatch_wrmsr(index, value),
            X86Exit::Cpuid { leaf, subleaf } => vmm.dispatch_cpuid(leaf, subleaf),
        }
    }

    fn dispatch_mmio<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        gpa: Gpa,
        size: u8,
        write: Option<u64>,
    ) -> Result<Step, VmmError> {
        vmm.dispatch_mmio(gpa, size, write)
    }

    fn is_doorbell_exit(exit: &Exit<Self>) -> bool {
        matches!(
            exit,
            Exit::Arch(X86Exit::Io {
                port: dispatch::DOORBELL_PORT,
                size: 4,
                write: Some(_),
            })
        )
    }

    fn normalize_virtual_time_exit(
        exit: &vmm_backend::Exit<Self>,
    ) -> Option<(crate::virtual_time::NormalizedEventClass, Vec<u8>)> {
        Some(dispatch::normalize_virtual_time_exit_x86(exit))
    }

    fn post_exit<B: Backend<A = Self>>(vmm: &mut Vmm<B>) -> Result<(), VmmError> {
        vmm.service_lapic_timer_due()
    }

    fn service_pending_irqs<B: Backend<A = Self>>(vmm: &mut Vmm<B>) -> Result<(), VmmError> {
        vmm.service_pending_irqs()
    }

    fn complete_irq_delivery<B: Backend<A = Self>>(vmm: &mut Vmm<B>) {
        vmm.complete_irq_delivery();
    }

    fn guest_interruptible<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Result<bool, VmmError> {
        Ok(vmm.backend().save()?.regs.rflags & dispatch::RFLAGS_IF != 0)
    }

    fn pending_deliverable_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
    ) -> Result<bool, VmmError> {
        Ok(vmm
            .devices()
            .lapic
            .as_ref()
            .is_some_and(|l| l.peek_interrupt().is_some()))
    }

    fn next_timer_deadline_vns<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Option<u64> {
        vmm.devices().lapic.as_ref()?.next_timer_deadline()
    }

    fn clockevent_trace_schedule<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Option<(u64, u32)> {
        let lapic = vmm.devices().lapic.as_ref()?;
        Some((
            lapic.next_timer_deadline()?,
            u32::from(lapic.timer_vector()),
        ))
    }

    fn deliverable_timer_deadline_vns<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Option<u64> {
        let lapic = vmm.devices().lapic.as_ref()?;
        lapic
            .next_timer_deadline()
            .filter(|_| lapic.armed_timer_deliverable())
    }

    fn check_wire_interrupt<B: Backend<A = Self>>(
        vmm: &Vmm<B>,
        vector: u32,
    ) -> Result<(), InterruptReject> {
        if vmm.devices().lapic.is_none() {
            return Err(InterruptReject::NoFabric);
        }
        let Ok(vector) = u8::try_from(vector) else {
            return Err(InterruptReject::OutOfRange);
        };
        if vector < 16 {
            return Err(InterruptReject::Reserved { vector });
        }
        Ok(())
    }

    fn inject_wire_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        vector: u32,
    ) -> Result<(), VmmError> {
        vmm.inject_host_interrupt(vector)
    }

    fn has_pending_guest_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
    ) -> Result<bool, VmmError> {
        vmm.has_pending_guest_interrupt_x86()
    }

    fn serial_capture(devices: &Self::Devices) -> &[u8] {
        devices.uart.capture()
    }

    fn inject_serial_input(devices: &mut Self::Devices, bytes: &[u8]) {
        devices.uart.inject_input(bytes);
    }

    fn encode_vcpu_chunk(vcpu: &vmm_backend::VcpuState) -> Vec<u8> {
        dispatch::encode_vcpu_state(vcpu)
    }

    fn encode_device_state(devices: &Self::Devices) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(devices.uart.shadow_regs());
        v.push(u8::from(devices.uart.dlab()));
        v
    }

    fn hash_device_chunks(
        _vcpu: &vmm_backend::VcpuState,
        devices: &Self::Devices,
        out: &mut Vec<u8>,
    ) {
        if let Some(lapic) = &devices.lapic {
            crate::vmm::put_chunk(
                out,
                b"LAPC",
                &dispatch::encode_lapic_state(&lapic.snapshot()),
            );
        }
        if let Some(legacy) = &devices.legacy {
            let mut legy = legacy.config_address().to_le_bytes().to_vec();
            legy.extend_from_slice(&legacy.pic_imr());
            crate::vmm::put_chunk(out, b"LEGY", &legy);
        }
    }

    fn regs_view(vcpu: &vmm_backend::VcpuState) -> RegsView {
        let r = &vcpu.regs;
        RegsView {
            version: RegsView::VERSION,
            gpr: [
                r.rax, r.rbx, r.rcx, r.rdx, r.rsi, r.rdi, r.rbp, r.rsp, r.r8, r.r9, r.r10, r.r11,
                r.r12, r.r13, r.r14, r.r15,
            ],
            rip: r.rip,
            rflags: r.rflags,
            seg: [
                vcpu.sregs.cs.selector,
                vcpu.sregs.ss.selector,
                vcpu.sregs.ds.selector,
                vcpu.sregs.es.selector,
                vcpu.sregs.fs.selector,
                vcpu.sregs.gs.selector,
            ],
            cr0: vcpu.sregs.cr0,
            cr3: vcpu.sregs.cr3,
            cr4: vcpu.sregs.cr4,
            moment: control_proto::Moment(0),
            vtime: 0,
        }
    }

    fn vcpu_components(vcpu: &vmm_backend::VcpuState, out: &mut Vec<(&'static str, [u8; 32])>) {
        dispatch::vcpu_components(vcpu, out);
    }

    fn vcpu_has_inflight_injection(vcpu: &vmm_backend::VcpuState) -> bool {
        records::has_inflight_injection(&vcpu.events)
    }

    fn vcpu_has_active_injection(vcpu: &vmm_backend::VcpuState) -> bool {
        records::has_active_event_injection(&vcpu.events)
    }

    fn check_sealable_vcpu(vcpu: &vmm_backend::VcpuState) -> Result<(), VmmError> {
        match records::unrepresentable_state(vcpu) {
            Some(reason) => Err(VmmError::ContractViolation(format!(
                "save_vm_state: {reason}"
            ))),
            None => Ok(()),
        }
    }

    fn build_vm_state<B: Backend<A = Self>>(
        vmm: &Vmm<B>,
        vcpu: &vmm_backend::VcpuState,
    ) -> VmState {
        vmm.build_vm_state(vcpu)
    }

    fn validate_restore<B: Backend<A = Self>>(
        vmm: &Vmm<B>,
        s: &VmState,
    ) -> Result<(vmm_backend::VcpuState, u64, Self::RestorePrep), VmmError> {
        vmm.validate_restore_x86(s)
    }

    fn commit_restore<B: Backend<A = Self>>(vmm: &mut Vmm<B>, prep: Self::RestorePrep) {
        vmm.commit_restore_x86(prep);
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn doorbell_classifier_accepts_only_the_exact_out() {
        let ring = Exit::Arch(X86Exit::Io {
            port: dispatch::DOORBELL_PORT,
            size: 4,
            write: Some(7),
        });
        assert!(<X86 as Vendor>::is_doorbell_exit(&ring));
        for exit in [
            Exit::Arch(X86Exit::Io {
                port: dispatch::DOORBELL_PORT,
                size: 1,
                write: Some(7),
            }),
            Exit::Arch(X86Exit::Io {
                port: dispatch::DOORBELL_PORT,
                size: 4,
                write: None,
            }),
            Exit::Arch(X86Exit::Io {
                port: dispatch::DOORBELL_PORT.wrapping_add(1),
                size: 4,
                write: Some(7),
            }),
        ] {
            assert!(!<X86 as Vendor>::is_doorbell_exit(&exit));
        }
    }
}
