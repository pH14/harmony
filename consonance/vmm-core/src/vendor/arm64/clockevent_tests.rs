// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;
use crate::vmm::{GuestRam, VtimeWiring};
use vmm_backend::{
    Arm64Completion, Arm64Injection, Arm64Policy, BackendError, Capabilities, ExitCounts, GicIntId,
    MockArm64Backend,
};

struct MaskBackend {
    inner: MockArm64Backend,
    mask: Option<bool>,
    fail: bool,
    reads: usize,
    saves: usize,
}

impl Backend for MaskBackend {
    type A = Arm64;
    unsafe fn map_memory(&mut self, _gpa: Gpa, _host: &mut [u8]) -> vmm_backend::Result<()> {
        Err(BackendError::Unsupported {
            what: "test memory mapping",
        })
    }
    fn read_irq_mask(&mut self) -> vmm_backend::Result<Option<bool>> {
        self.reads += 1;
        if self.fail {
            Err(BackendError::PendingCompletion)
        } else {
            Ok(self.mask)
        }
    }
    fn save(&mut self) -> vmm_backend::Result<Arm64VcpuState> {
        self.saves += 1;
        self.inner.save()
    }
    fn set_policy(&mut self, policy: &Arm64Policy) -> vmm_backend::Result<()> {
        self.inner.set_policy(policy)
    }
    fn run(&mut self) -> vmm_backend::Result<Exit<Arm64>> {
        self.inner.run()
    }
    fn inject(&mut self, event: Arm64Injection) -> vmm_backend::Result<()> {
        self.inner.inject(event)
    }
    fn set_pending_irq(&mut self, id: Option<GicIntId>) -> vmm_backend::Result<()> {
        self.inner.set_pending_irq(id)
    }
    fn take_accepted_interrupt(&mut self) -> Option<GicIntId> {
        self.inner.take_accepted_interrupt()
    }
    fn complete_read(&mut self, value: u64) -> vmm_backend::Result<()> {
        self.inner.complete_read(value)
    }
    fn complete_fault(&mut self) -> vmm_backend::Result<()> {
        self.inner.complete_fault()
    }
    fn complete_ok(&mut self) -> vmm_backend::Result<()> {
        self.inner.complete_ok()
    }
    fn complete_hypercall(&mut self, ret: u64) -> vmm_backend::Result<()> {
        self.inner.complete_hypercall(ret)
    }
    fn complete_arch(&mut self, completion: Arm64Completion) -> vmm_backend::Result<()> {
        self.inner.complete_arch(completion)
    }
    fn restore(&mut self, state: &Arm64VcpuState) -> vmm_backend::Result<()> {
        self.inner.restore(state)
    }
    fn reset_exit_counts(&mut self) {
        self.inner.reset_exit_counts()
    }
    fn capabilities(&self) -> Capabilities<vmm_backend::Arm64Caps> {
        self.inner.capabilities()
    }
    fn exit_counts(&self) -> ExitCounts {
        self.inner.exit_counts()
    }
}

fn machine(mask: Option<bool>, pstate_masked: bool) -> Vmm<MaskBackend> {
    let mut inner = MockArm64Backend::new();
    inner.set_policy(&Arm64Policy::default()).unwrap();
    let mut state = Arm64VcpuState::default();
    state.core.pstate = u64::from(pstate_masked) << 7;
    inner.set_state(state);
    let mut vmm = Vmm::new(
        MaskBackend {
            inner,
            mask,
            fail: false,
            reads: 0,
            saves: 0,
        },
        GuestRam::new(0x4000).unwrap(),
    );
    vmm.wire_vtime(
        VtimeWiring::new_virtual_time(
            vtime::VClockConfig {
                guest_hz: super::super::board::CNTFRQ_HZ,
                guest_base: 0,
                vns_base: 0,
            },
            7,
        )
        .unwrap(),
    );
    vmm.enable_pvclock();
    assert_eq!(vmm.pvclock_register(0x1000).0, Status::Ok);
    vmm.wire_gic(super::super::board::new_gic());
    vmm.devices.clockevent.deadline = Some(0);
    let trace = vmm.virtual_time_trace.as_mut().unwrap();
    trace
        .restore_clockevent_schedule(Some((0, super::super::board::PVCLOCK_PPI)))
        .unwrap();
    trace
        .begin(
            vmm_backend::ExitReason::Mmio,
            "clockevent test".into(),
            NormalizedEventClass::DeviceMmio(DeviceClass::Paravirtual),
            vec![],
        )
        .unwrap();
    vmm
}

#[test]
fn due_clockevent_uses_direct_mask_without_full_capture() {
    for masked in [false, true] {
        let mut fast = machine(Some(masked), !masked);
        let mut fallback = machine(None, masked);
        fast.service_arm_clockevent_due().unwrap();
        fallback.service_arm_clockevent_due().unwrap();
        fast.virtual_time_trace
            .as_mut()
            .unwrap()
            .finish(0, None)
            .unwrap();
        fallback
            .virtual_time_trace
            .as_mut()
            .unwrap()
            .finish(0, None)
            .unwrap();
        let trace = fast.virtual_time_trace().unwrap();
        let reference = fallback.virtual_time_trace().unwrap();
        assert_eq!(trace.normalized_log(), reference.normalized_log());
        assert_eq!(trace.schedule(), reference.schedule());
        assert_eq!(trace.schedule().len(), if masked { 2 } else { 1 });
        assert_eq!(
            trace.normalized_log().events[0].interrupts.len(),
            usize::from(!masked)
        );
        assert_eq!(fast.devices.clockevent, fallback.devices.clockevent);
        assert_eq!(fast.devices.clockevent.line_asserted, !masked);
        assert_eq!(fast.devices.clockevent.deadline, masked.then_some(0));
        assert_eq!(
            fast.devices.gic.as_ref().unwrap().snapshot(),
            fallback.devices.gic.as_ref().unwrap().snapshot()
        );
        assert_eq!((fast.backend.reads, fast.backend.saves), (1, 0));
        assert_eq!((fallback.backend.reads, fallback.backend.saves), (1, 1));
    }
}

#[test]
fn irq_mask_error_preserves_due_timer_without_capture_or_delivery() {
    let mut vmm = machine(Some(false), false);
    vmm.backend.fail = true;
    let before = vmm.devices.clockevent;
    let gic = vmm.devices.gic.as_ref().unwrap().snapshot();
    assert!(matches!(
        vmm.service_arm_clockevent_due(),
        Err(VmmError::Backend(BackendError::PendingCompletion))
    ));
    assert_eq!(vmm.devices.clockevent, before);
    assert_eq!(vmm.devices.gic.as_ref().unwrap().snapshot(), gic);
    assert_eq!((vmm.backend.reads, vmm.backend.saves), (1, 0));
}

#[test]
fn absent_or_future_timer_does_not_query_the_backend() {
    for deadline in [None, Some(1)] {
        let mut vmm = machine(Some(false), false);
        vmm.backend.fail = true;
        vmm.devices.clockevent.deadline = deadline;
        vmm.service_arm_clockevent_due().unwrap();
        assert_eq!((vmm.backend.reads, vmm.backend.saves), (0, 0));
        assert_eq!(vmm.devices.clockevent.deadline, deadline);
    }
}
