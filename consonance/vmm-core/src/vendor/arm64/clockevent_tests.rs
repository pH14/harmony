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

#[test]
fn idle_to_timer_transition_uses_direct_mask_for_both_checks() {
    use super::super::board::{CNTFRQ_HZ, PVCLOCK_PPI};
    use crate::vmm::TerminalReason;
    const DEADLINE: u64 = 1_000_000;
    for masked in [false, true] {
        let mut fast = machine(Some(masked), !masked);
        let mut fallback = machine(None, masked);
        for vmm in [&mut fast, &mut fallback] {
            let gic = vmm.devices.gic.as_mut().unwrap();
            gic.mmio_write(gicv3::GicFrame::Dist, 0, 0b10, 0).unwrap();
            gic.mmio_write(gicv3::GicFrame::Redist, 0x1_0080, 1 << PVCLOCK_PPI, 0)
                .unwrap();
            gic.mmio_write(gicv3::GicFrame::Redist, 0x1_0100, 1 << PVCLOCK_PPI, 0)
                .unwrap();
            gic.set_pmr(0xff);
            gic.set_group1_enabled(true);
            vmm.devices.clockevent.deadline = Some(CNTFRQ_HZ / 1000);
            let trace = vmm.virtual_time_trace.as_mut().unwrap();
            trace.finish(0, None).unwrap();
            trace
                .restore_clockevent_schedule(Some((DEADLINE, PVCLOCK_PPI)))
                .unwrap();
            trace
                .begin(
                    vmm_backend::ExitReason::Idle,
                    "idle".into(),
                    NormalizedEventClass::Idle,
                    vec![],
                )
                .unwrap();
            assert_eq!(
                vmm.on_idle().unwrap(),
                if masked {
                    Step::Terminal(TerminalReason::Idle)
                } else {
                    Step::Continued
                }
            );
            vmm.service_arm_clockevent_due().unwrap();
            let vns = vmm.effective_vns().unwrap();
            assert_eq!(vns, if masked { 0 } else { DEADLINE });
            vmm.virtual_time_trace
                .as_mut()
                .unwrap()
                .finish(vns, None)
                .unwrap();
        }
        assert_eq!(
            fast.idle_landings(),
            if masked { &[][..] } else { &[DEADLINE][..] }
        );
        assert_eq!(fast.idle_landings(), fallback.idle_landings());
        assert_eq!(fast.devices.clockevent, fallback.devices.clockevent);
        assert_eq!(fast.devices.clockevent.line_asserted, !masked);
        assert_eq!(
            fast.virtual_time_trace().unwrap().normalized_log(),
            fallback.virtual_time_trace().unwrap().normalized_log()
        );
        assert_eq!(
            fast.virtual_time_trace().unwrap().schedule(),
            fallback.virtual_time_trace().unwrap().schedule()
        );
        let polls = if masked { 1 } else { 2 };
        assert_eq!((fast.backend.reads, fast.backend.saves), (polls, 0));
        assert_eq!(
            (fallback.backend.reads, fallback.backend.saves),
            (polls, polls)
        );
    }
}

#[test]
fn idle_irq_mask_error_does_not_advance_time_or_fall_back() {
    let mut vmm = machine(Some(false), false);
    vmm.backend.fail = true;
    assert!(matches!(
        vmm.on_idle(),
        Err(VmmError::Backend(BackendError::PendingCompletion))
    ));
    assert_eq!(vmm.effective_vns(), Some(0));
    assert!(vmm.idle_landings().is_empty());
    assert_eq!(vmm.terminal_reason(), None);
    assert_eq!((vmm.backend.reads, vmm.backend.saves), (1, 0));
}

#[test]
#[cfg_attr(miri, ignore = "control snapshot replay uses mapped storage")]
fn raw_exit_delivers_after_unmask_at_unchanged_clock_and_records_the_boundary() {
    use super::super::board::{GICD, PL011, PVCLOCK_PPI};
    use vmm_backend::Arm64MockCompletion;
    const CUT_VNS: u64 = 10 + 254 * 10_000;
    let mut reference_history = None;
    for exit in [
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(GICD.0),
            size: 4,
            write: None,
        }),
        Exit::Arch(vmm_backend::Arm64Exit::Sysreg {
            sysreg: OSLAR_EL1,
            write: Some(0),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(GICD.0),
            size: 4,
            write: Some(0b10),
        }),
    ] {
        let mut vmm = machine(Some(true), true);
        vmm.deferred_virtual_time_checkpoints = true;
        let gic = vmm.devices.gic.as_mut().unwrap();
        gic.mmio_write(gicv3::GicFrame::Dist, 0, 0b10, 0).unwrap();
        gic.mmio_write(gicv3::GicFrame::Redist, 0x1_0080, 1 << PVCLOCK_PPI, 0)
            .unwrap();
        gic.mmio_write(gicv3::GicFrame::Redist, 0x1_0100, 1 << PVCLOCK_PPI, 0)
            .unwrap();
        gic.set_pmr(0xff);
        gic.set_group1_enabled(true);
        vmm.vtime.as_mut().unwrap().advance_virtual_time(10);
        vmm.service_arm_clockevent_due().unwrap();
        vmm.virtual_time_trace
            .as_mut()
            .unwrap()
            .finish(10, None)
            .unwrap();
        for _ in 1..255 {
            vmm.backend
                .inner
                .extend_exits([Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(PL011.0),
                    size: 4,
                    write: Some(65),
                })]);
            assert_eq!(vmm.step().unwrap(), Step::Continued);
        }
        assert!(!vmm.devices.clockevent.line_asserted);
        assert_eq!(vmm.devices.gic.as_ref().unwrap().peek_interrupt(), None);
        let history = vmm.event_history().unwrap();
        vmm.backend.inner.extend_exits([exit.clone()]);
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        assert_eq!(vmm.event_history().unwrap(), history);
        assert_eq!(vmm.effective_vns().unwrap(), CUT_VNS);
        let raw_before = vmm.virtual_time_trace().unwrap().raw_log().len();
        vmm.backend.mask = Some(false);
        vmm.backend.inner.set_state(Arm64VcpuState::default());
        vmm.backend.inner.extend_exits([exit.clone()]);
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        assert_eq!(vmm.effective_vns().unwrap(), CUT_VNS);
        assert!(vmm.devices.clockevent.line_asserted);
        assert_eq!(vmm.devices.clockevent.assertions, 1);
        assert_eq!(vmm.devices.clockevent.deadline, None);
        assert_eq!(
            vmm.devices.gic.as_ref().unwrap().peek_interrupt(),
            Some(PVCLOCK_PPI)
        );
        if matches!(exit, Exit::Common(CommonExit::Mmio { write: None, .. })) {
            assert_eq!(
                vmm.backend.inner.completions().last(),
                Some(&Arm64MockCompletion::Read(0x52))
            );
        }
        let trace = vmm.virtual_time_trace().unwrap();
        assert_eq!(trace.event_history().count, history.count + 1);
        let event = trace.normalized_log().events.last().unwrap();
        assert_eq!(event.class, NormalizedEventClass::InterruptDelivery);
        assert_eq!(event.vns_after, CUT_VNS);
        assert_eq!(event.interrupts.len(), 1);
        assert_eq!(event.interrupts[0].interrupt_id, PVCLOCK_PPI);
        assert_eq!(trace.raw_log().len(), raw_before + 1);
        assert!(vmm.virtual_time_checkpoint_due(255));
        assert_eq!(event.state_hash, None);
        crate::virtual_time::check_delivery_placement(trace.schedule(), trace.normalized_log())
            .unwrap();
        let committed = vmm.event_history().unwrap();
        if let Some(expected) = reference_history {
            assert_eq!(committed, expected);
        } else {
            reference_history = Some(committed);
        }
        vmm.checkpoint_virtual_time_trace_at(255, [42; 32]).unwrap();
        assert_eq!(vmm.event_history(), Some(committed));
        vmm.wire_snapshot_hashing();
        let mut server = crate::control::ControlServer::new(
            vmm,
            Box::new(|| {
                let mut fresh = machine(Some(false), false);
                fresh
                    .virtual_time_trace
                    .as_mut()
                    .unwrap()
                    .finish(0, None)
                    .unwrap();
                fresh.wire_snapshot_hashing();
                Ok(fresh)
            }),
        );
        assert!(matches!(
            server
                .handle(&control_proto::Request::Hello(crate::control::server_caps()))
                .unwrap(),
            Ok(control_proto::Reply::Hello(_))
        ));
        if matches!(exit, Exit::Common(CommonExit::Mmio { write: Some(_), .. })) {
            let snap = match server
                .handle(&control_proto::Request::Snapshot)
                .unwrap()
                .unwrap()
            {
                control_proto::Reply::Snapshot { id, .. } => id,
                other => panic!("unexpected snapshot reply {other:?}"),
            };
            assert_eq!(server.vmm().unwrap().event_history(), Some(committed));
            server
                .vmm_mut()
                .unwrap()
                .restore_event_history(Default::default())
                .unwrap();
            assert_eq!(
                server
                    .handle(&control_proto::Request::Replay(snap))
                    .unwrap(),
                Ok(control_proto::Reply::Unit)
            );
        }
        let vmm = server.vmm_mut().unwrap();
        assert_eq!(vmm.event_history(), Some(committed));
        assert_eq!(vmm.effective_vns().unwrap(), CUT_VNS);
        assert!(vmm.devices.clockevent.line_asserted);
        assert_eq!(vmm.devices.clockevent.deadline, None);
        assert_eq!(
            vmm.devices.gic.as_ref().unwrap().peek_interrupt(),
            Some(PVCLOCK_PPI)
        );
        vmm.backend
            .inner
            .extend_exits([Exit::Common(CommonExit::Mmio {
                gpa: Gpa(PL011.0),
                size: 4,
                write: Some(65),
            })]);
        assert_eq!(vmm.step().unwrap(), Step::Continued);
        assert_eq!(vmm.effective_vns().unwrap(), CUT_VNS + 10_000);
        assert_eq!(vmm.devices.clockevent.assertions, 1);
        assert_eq!(vmm.event_history().unwrap().count, history.count + 2);
    }
}
