// SPDX-License-Identifier: AGPL-3.0-or-later

use control_proto::RegsView;
use vmm_backend::{Arch, Backend, Exit, Gpa};

use crate::virtual_time::NormalizedEventClass;
use crate::vmm::{Step, Vmm, VmmError};

pub mod arm64;
pub mod x86;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InterruptReject {
    NoFabric,
    OutOfRange,
    Reserved { vector: u8 },
}

pub trait Vendor: Arch + Sized {
    type Devices;

    type RestorePrep;

    type Snapshot: vm_state::SnapshotRecords;

    fn new_devices() -> Self::Devices;

    fn mmio_holes() -> &'static [(u64, u64)];

    fn dispatch_arch<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        exit: Self::Exit,
    ) -> Result<Step, VmmError>;

    fn dispatch_mmio<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        gpa: Gpa,
        size: u8,
        write: Option<u64>,
    ) -> Result<Step, VmmError>;

    fn finish_exit<B: Backend<A = Self>>(
        _vmm: &mut Vmm<B>,
    ) -> Result<Option<Exit<Self>>, VmmError> {
        Ok(None)
    }

    fn is_doorbell_exit(exit: &Exit<Self>) -> bool;

    fn post_exit<B: Backend<A = Self>>(_vmm: &mut Vmm<B>) -> Result<(), VmmError> {
        Ok(())
    }

    fn normalize_virtual_time_exit(_exit: &Exit<Self>) -> Option<(NormalizedEventClass, Vec<u8>)> {
        None
    }

    fn service_pending_irqs<B: Backend<A = Self>>(vmm: &mut Vmm<B>) -> Result<(), VmmError>;

    fn complete_irq_delivery<B: Backend<A = Self>>(vmm: &mut Vmm<B>);

    fn guest_interruptible<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Result<bool, VmmError>;

    fn pending_deliverable_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
    ) -> Result<bool, VmmError>;

    fn next_timer_deadline_vns<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Option<u64>;

    fn clockevent_trace_schedule<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Option<(u64, u32)>;

    fn deliverable_timer_deadline_vns<B: Backend<A = Self>>(vmm: &Vmm<B>) -> Option<u64>;

    fn check_wire_interrupt<B: Backend<A = Self>>(
        vmm: &Vmm<B>,
        vector: u32,
    ) -> Result<(), InterruptReject>;

    fn inject_wire_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
        vector: u32,
    ) -> Result<(), VmmError>;

    fn has_pending_guest_interrupt<B: Backend<A = Self>>(
        vmm: &mut Vmm<B>,
    ) -> Result<bool, VmmError>;

    fn serial_capture(devices: &Self::Devices) -> &[u8];

    fn inject_serial_input(devices: &mut Self::Devices, bytes: &[u8]);

    fn encode_vcpu_chunk(vcpu: &Self::VcpuState) -> Vec<u8>;

    fn encode_device_state(devices: &Self::Devices) -> Vec<u8>;

    fn hash_device_chunks(vcpu: &Self::VcpuState, devices: &Self::Devices, out: &mut Vec<u8>);

    fn regs_view(vcpu: &Self::VcpuState) -> RegsView;

    fn vcpu_components(vcpu: &Self::VcpuState, out: &mut Vec<(&'static str, [u8; 32])>);

    fn device_components(
        _vcpu: &Self::VcpuState,
        _devices: &Self::Devices,
        _out: &mut Vec<(&'static str, [u8; 32])>,
    ) {
    }

    fn vcpu_has_inflight_injection(vcpu: &Self::VcpuState) -> bool;

    fn vcpu_has_active_injection(vcpu: &Self::VcpuState) -> bool;

    fn check_sealable_vcpu(vcpu: &Self::VcpuState) -> Result<(), VmmError>;

    fn build_vm_state<B: Backend<A = Self>>(vmm: &Vmm<B>, vcpu: &Self::VcpuState)
    -> Self::Snapshot;

    fn validate_restore<B: Backend<A = Self>>(
        vmm: &Vmm<B>,
        s: &Self::Snapshot,
    ) -> Result<(Self::VcpuState, u64, Self::RestorePrep), VmmError>;

    fn commit_restore<B: Backend<A = Self>>(vmm: &mut Vmm<B>, prep: Self::RestorePrep);
}
