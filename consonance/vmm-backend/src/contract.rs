// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::arch::x86::{X86, X86Completion, X86Exit, X86Policy};
use crate::backend::Backend;
use crate::error::BackendError;
use crate::exit::{CommonExit, Exit};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scenario {
    Idle,
    PortIn,
    MmioLoad,
    Rdmsr,
    Wrmsr,
    Cpuid,
    Hypercall,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PendingKind {
    Read,
    Rdmsr,
    Wrmsr,
    Hypercall,
    Cpuid,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Completion {
    Read,
    Fault,
    Ok,
    Hypercall,
    Arch,
}

const ALL_COMPLETIONS: [Completion; 5] = [
    Completion::Read,
    Completion::Fault,
    Completion::Ok,
    Completion::Hypercall,
    Completion::Arch,
];

impl Completion {
    fn resolves(self, pending: PendingKind) -> bool {
        matches!(
            (pending, self),
            (PendingKind::Read, Completion::Read)
                | (PendingKind::Rdmsr, Completion::Read | Completion::Fault)
                | (PendingKind::Wrmsr, Completion::Ok | Completion::Fault)
                | (PendingKind::Hypercall, Completion::Hypercall)
                | (PendingKind::Cpuid, Completion::Arch)
        )
    }

    fn apply<B: Backend<A = X86>>(self, backend: &mut B) -> crate::error::Result<()> {
        match self {
            Completion::Read => backend.complete_read(0),
            Completion::Fault => backend.complete_fault(),
            Completion::Ok => backend.complete_ok(),
            Completion::Hypercall => backend.complete_hypercall(0),
            Completion::Arch => backend.complete_arch(X86Completion::Cpuid {
                eax: 0,
                ebx: 0,
                ecx: 0,
                edx: 0,
            }),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Completion::Read => "complete_read",
            Completion::Fault => "complete_fault",
            Completion::Ok => "complete_ok",
            Completion::Hypercall => "complete_hypercall",
            Completion::Arch => "complete_arch",
        }
    }
}

pub trait BackendFixture {
    type B: Backend<A = X86>;

    fn name(&self) -> &'static str;

    fn spawn(&mut self, scenario: Scenario) -> Option<Self::B>;

    fn policy(&self) -> X86Policy;

    fn dirty_pages(&mut self, backend: &mut Self::B) -> Option<Vec<u64>> {
        let _ = backend;
        None
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DeclineReason {
    ScenarioUnavailable(Scenario),
    NoDirtyLog,
    CapabilityAbsent(&'static str),
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Decline {
    pub exam: &'static str,
    pub why: DeclineReason,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ContractReport {
    pub backend: &'static str,
    pub ran: Vec<&'static str>,
    pub declined: Vec<Decline>,
}

impl ContractReport {
    fn new(backend: &'static str) -> Self {
        ContractReport {
            backend,
            ran: Vec::new(),
            declined: Vec::new(),
        }
    }

    fn ran(&mut self, exam: &'static str) {
        self.ran.push(exam);
    }

    fn decline(&mut self, exam: &'static str, why: DeclineReason) {
        self.declined.push(Decline { exam, why });
    }

    pub fn did_run(&self, exam: &'static str) -> bool {
        self.ran.contains(&exam)
    }
}

fn scenario_for(kind: PendingKind) -> Scenario {
    match kind {
        PendingKind::Read => Scenario::PortIn,
        PendingKind::Rdmsr => Scenario::Rdmsr,
        PendingKind::Wrmsr => Scenario::Wrmsr,
        PendingKind::Hypercall => Scenario::Hypercall,
        PendingKind::Cpuid => Scenario::Cpuid,
    }
}

#[track_caller]
fn assert_exit_matches(kind: PendingKind, exit: &Exit<X86>) {
    let ok = match kind {
        PendingKind::Read => matches!(
            exit,
            Exit::Arch(X86Exit::Io { write: None, .. })
                | Exit::Common(CommonExit::Mmio { write: None, .. })
        ),
        PendingKind::Rdmsr => matches!(exit, Exit::Arch(X86Exit::Rdmsr { .. })),
        PendingKind::Wrmsr => matches!(exit, Exit::Arch(X86Exit::Wrmsr { .. })),
        PendingKind::Hypercall => matches!(exit, Exit::Common(CommonExit::Hypercall(_))),
        PendingKind::Cpuid => matches!(exit, Exit::Arch(X86Exit::Cpuid { .. })),
    };
    assert!(
        ok,
        "fixture armed {kind:?} but the backend returned {exit:?}"
    );
}

pub fn ordering_exam<F: BackendFixture>(fx: &mut F, report: &mut ContractReport) {
    match fx.spawn(Scenario::Idle) {
        None => report.decline(
            "ordering/not_configured",
            DeclineReason::ScenarioUnavailable(Scenario::Idle),
        ),
        Some(mut b) => {
            assert!(
                matches!(b.run(), Err(BackendError::NotConfigured)),
                "run before set_policy must be NotConfigured"
            );
            report.ran("ordering/not_configured");
        }
    }

    let mut grid_ran = false;
    for kind in [
        PendingKind::Read,
        PendingKind::Rdmsr,
        PendingKind::Wrmsr,
        PendingKind::Hypercall,
        PendingKind::Cpuid,
    ] {
        let scenario = scenario_for(kind);
        let Some(mut b) = fx.spawn(scenario) else {
            report.decline(
                "ordering/completion_grid",
                DeclineReason::ScenarioUnavailable(scenario),
            );
            continue;
        };
        b.set_policy(&fx.policy()).expect("set_policy");
        let exit = b.run().expect("run to the armed exit");
        assert_exit_matches(kind, &exit);

        assert!(
            matches!(b.run(), Err(BackendError::PendingCompletion)),
            "{kind:?}: resuming with an unserviced exit must be PendingCompletion"
        );

        for method in ALL_COMPLETIONS {
            if method.resolves(kind) {
                continue;
            }
            let got = method.apply(&mut b);
            match method {
                Completion::Read => assert!(
                    matches!(got, Err(BackendError::NoPendingRead)),
                    "{kind:?} × complete_read must be NoPendingRead, got {got:?}"
                ),
                Completion::Fault | Completion::Ok | Completion::Arch => assert!(
                    matches!(got, Err(BackendError::BadCompletion)),
                    "{kind:?} × {} must be BadCompletion, got {got:?}",
                    method.name()
                ),
                Completion::Hypercall => assert!(
                    matches!(
                        got,
                        Err(BackendError::BadCompletion | BackendError::NoPendingRead)
                    ),
                    "{kind:?} × complete_hypercall must error, got {got:?}"
                ),
            }
            assert!(
                matches!(b.run(), Err(BackendError::PendingCompletion)),
                "{kind:?}: a rejected {} must leave the exit pending",
                method.name()
            );
        }

        let correct = ALL_COMPLETIONS
            .into_iter()
            .find(|m| m.resolves(kind))
            .expect("every pending kind has a resolving completion");
        correct
            .apply(&mut b)
            .expect("the correct completion must succeed");
        b.run().expect("resume after a correct completion");
        grid_ran = true;
    }
    if grid_ran {
        report.ran("ordering/completion_grid");
    }
}

pub fn dirty_log_exactness_exam<F: BackendFixture>(fx: &mut F, report: &mut ContractReport) {
    let Some(mut b) = fx.spawn(Scenario::Idle) else {
        report.decline(
            "exactness/dirty_log",
            DeclineReason::ScenarioUnavailable(Scenario::Idle),
        );
        return;
    };
    b.set_policy(&fx.policy()).expect("set_policy");
    let Some(expected) = fx.dirty_pages(&mut b) else {
        assert!(
            matches!(b.drain_dirty_pages(), Err(BackendError::Unsupported { .. })),
            "a backend with no dirty log must answer Unsupported, never an Ok set it cannot \
             vouch for"
        );
        report.ran("exactness/dirty_log_declines_loudly");
        report.decline("exactness/dirty_log", DeclineReason::NoDirtyLog);
        return;
    };

    let drained = b
        .drain_dirty_pages()
        .expect("a fixture that staged dirty pages must have a working log");

    assert!(
        drained.windows(2).all(|w| w[0] < w[1]),
        "the dirty set must be sorted ascending and deduplicated, got {drained:?}"
    );
    for gfn in &expected {
        assert!(
            drained.contains(gfn),
            "gfn {gfn} was written but is missing from {drained:?}: an under-report is \
             silent snapshot corruption"
        );
    }

    let again = b.drain_dirty_pages().expect("second drain");
    assert!(
        again.is_empty(),
        "the log must reset on drain; a second drain replayed {again:?}"
    );
    report.ran("exactness/dirty_log");
}

pub fn fixpoint_exam<F: BackendFixture>(fx: &mut F, report: &mut ContractReport) {
    let Some(mut b) = fx.spawn(Scenario::Idle) else {
        report.decline(
            "fixpoint/save_restore_save",
            DeclineReason::ScenarioUnavailable(Scenario::Idle),
        );
        return;
    };
    b.set_policy(&fx.policy()).expect("set_policy");

    let first = b.save().expect("save");
    b.restore(&first)
        .expect("restore a state this backend produced");
    let second = b.save().expect("save after restore");
    assert_eq!(
        first, second,
        "save -> restore -> save must be a fixpoint: a field dropped by the round trip is a \
         field a snapshot silently loses"
    );

    let mut malformed = first.clone();
    malformed.sregs.cs.limit = u32::MAX;
    malformed.sregs.cs.selector = u16::MAX;
    match b.restore(&malformed) {
        Ok(()) | Err(BackendError::InvalidState) => {}
        Err(other) => panic!("a malformed VcpuState must be InvalidState, got {other:?}"),
    }
    report.ran("fixpoint/save_restore_save");
}

pub fn interrupt_delivery_exam<F: BackendFixture>(fx: &mut F, report: &mut ContractReport) {
    let Some(mut b) = fx.spawn(Scenario::Idle) else {
        report.decline(
            "interrupts/one_overwritable_slot",
            DeclineReason::ScenarioUnavailable(Scenario::Idle),
        );
        return;
    };
    b.set_policy(&fx.policy()).expect("set_policy");
    if let Err(BackendError::Unsupported { .. }) = b.set_pending_irq(Some(0x30)) {
        report.decline(
            "interrupts/one_overwritable_slot",
            DeclineReason::CapabilityAbsent("maskable interrupt delivery"),
        );
        return;
    }
    assert!(
        b.take_accepted_interrupt().is_none(),
        "an identity that has only been staged must not be reported as accepted"
    );
    drop(b);

    let mut b = fx.spawn(Scenario::Idle).expect("idle");
    b.set_policy(&fx.policy()).expect("set_policy");
    b.set_pending_irq(Some(0x30)).expect("set_pending_irq");
    b.set_pending_irq(Some(0x41)).expect("overwrite");
    b.run().expect("enter the guest");
    assert_eq!(
        b.take_accepted_interrupt(),
        Some(0x41),
        "the slot must hold the LAST identity set, not the first"
    );
    assert!(
        b.take_accepted_interrupt().is_none(),
        "the slot is one identity, not a queue: the overwritten identity must not resurface"
    );
    drop(b);

    let mut b = fx.spawn(Scenario::Idle).expect("idle");
    b.set_policy(&fx.policy()).expect("set_policy");
    b.set_pending_irq(Some(0x30)).expect("set_pending_irq");
    b.set_pending_irq(None).expect("clear");
    b.run().expect("enter the guest");
    assert!(
        b.take_accepted_interrupt().is_none(),
        "set_pending_irq(None) must clear the slot, so nothing is accepted"
    );
    report.ran("interrupts/one_overwritable_slot");
}

pub fn run_all<F: BackendFixture>(fx: &mut F) -> ContractReport {
    let mut report = ContractReport::new(fx.name());
    ordering_exam(fx, &mut report);
    dirty_log_exactness_exam(fx, &mut report);
    fixpoint_exam(fx, &mut report);
    interrupt_delivery_exam(fx, &mut report);
    report
}
