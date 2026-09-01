// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **portable** leg of the `Backend` contract tests (`docs/TESTING.md`,
//! rung 2): the full [`vmm_backend::contract`] exam over [`MockBackend`], in the
//! ordinary `cargo nextest` lane on macOS and Linux.
//!
//! The box-only leg (`tests/contract_kvm.rs`) runs the **identical** exam over
//! `KvmBackend` and `PatchedKvmBackend`. That is the point of the suite: not
//! that the mock behaves, but that the mock and the live backends behave the
//! same, so vmm-core can be written against the trait alone.
#![cfg(all(feature = "contract-tests", feature = "mock"))]

use vmm_backend::contract::{
    BackendFixture, ContractReport, Decline, DeclineReason, Scenario, run_all,
};
use vmm_backend::{
    Backend, Capabilities, CommonExit, CpuidModel, Exit, ExitCounts, Gpa, HypercallFrame,
    Injection, MockBackend, MockCaps, MsrFilter, Result, VcpuState, X86, X86Caps, X86Completion,
    X86Exit, X86Policy,
};

/// How many `Idle` exits every script ends with. The exam resumes a backend
/// after servicing an exit, and the interrupt exams enter the guest once per
/// spawn, so a scripted mock needs a few halts in reserve; a live guest gets the
/// same shape from a `hlt`-loop stub.
const IDLE_TAIL: usize = 6;

fn idle_tail() -> Vec<Exit<X86>> {
    vec![Exit::Common(CommonExit::Idle); IDLE_TAIL]
}

/// The scripted exits that put a `MockBackend` in each [`Scenario`].
fn script(scenario: Scenario) -> Vec<Exit<X86>> {
    let head: Vec<Exit<X86>> = match scenario {
        Scenario::Idle => Vec::new(),
        Scenario::PortIn => vec![Exit::Arch(X86Exit::Io {
            port: 0x3F8,
            size: 1,
            write: None,
        })],
        Scenario::MmioLoad => vec![Exit::Common(CommonExit::Mmio {
            gpa: vmm_backend::Gpa(0xFEE0_0000),
            size: 4,
            write: None,
        })],
        Scenario::Rdmsr => vec![Exit::Arch(X86Exit::Rdmsr { index: 0x1234_5678 })],
        Scenario::Wrmsr => vec![Exit::Arch(X86Exit::Wrmsr {
            index: 0x1234_5678,
            value: 0xDEAD_BEEF,
        })],
        Scenario::Cpuid => vec![Exit::Arch(X86Exit::Cpuid {
            leaf: 0,
            subleaf: 0,
        })],
        Scenario::Hypercall => vec![Exit::Common(CommonExit::Hypercall(HypercallFrame {
            args: [0x3150_4348, 0xE000, 0xF000, 0],
        }))],
        Scenario::Rdtsc => vec![Exit::Arch(X86Exit::Rdtsc)],
        Scenario::Rdrand => vec![Exit::Arch(X86Exit::Rdrand { width: 8 })],
    };
    let mut exits = head;
    exits.extend(idle_tail());
    exits
}

/// The mock fixture. Owns nothing beyond the scripts: the mock records regions
/// rather than retaining host pointers, so there is no guest memory to keep
/// alive around a backend.
struct MockFixture;

impl BackendFixture for MockFixture {
    type B = MockBackend;

    fn name(&self) -> &'static str {
        "mock"
    }

    fn spawn(&mut self, scenario: Scenario) -> Option<MockBackend> {
        // The mock can produce every scenario — it is a controlled in-process
        // model, and its advertised capabilities say so.
        Some(MockBackend::with_exits(script(scenario)))
    }

    fn policy(&self) -> X86Policy {
        X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        }
    }

    fn dirty_pages(&mut self, backend: &mut MockBackend) -> Option<Vec<u64>> {
        // Deliberately unsorted and duplicated: the trait requires the backend
        // to answer sorted-and-deduplicated whatever the writes looked like.
        backend.push_dirty_gfns(vec![9, 2, 4, 2, 9]);
        Some(vec![2, 4, 9])
    }
}

/// The exams the mock must run. Named individually rather than counted: a
/// renamed or dropped exam has to fail here, not silently shrink the suite.
const REQUIRED: &[&str] = &[
    "ordering/not_configured",
    "ordering/completion_grid",
    "exactness/dirty_log",
    "exactness/deterministic_tsc_traps",
    "exactness/deterministic_rng_traps",
    "fixpoint/save_restore_save",
    "interrupts/one_overwritable_slot",
];

#[test]
fn mock_backend_passes_the_full_contract_exam() {
    let mut fx = MockFixture;
    let report: ContractReport = run_all(&mut fx);

    assert_eq!(report.backend, "mock");
    for exam in REQUIRED {
        assert!(
            report.did_run(exam),
            "{exam} did not run against the mock: {report:?}"
        );
    }
    // The mock is the one backend with no honest excuse: it models every
    // scenario and advertises every determinism capability, so a decline here
    // means an exam quietly stopped examining.
    assert!(
        report.declined.is_empty(),
        "the mock must decline nothing: {:?}",
        report.declined
    );
}

/// Non-vacuity guard for the whole exam: a backend that breaks a contract must
/// fail it. `BrokenFixture` hands out a mock whose policy is installed *before*
/// the exam gets it, so `run` before `set_policy` no longer fails closed — the
/// first thing `ordering_exam` checks.
struct BrokenFixture;

impl BackendFixture for BrokenFixture {
    type B = MockBackend;

    fn name(&self) -> &'static str {
        "mock-preconfigured"
    }

    fn spawn(&mut self, scenario: Scenario) -> Option<MockBackend> {
        let mut b = MockBackend::with_exits(script(scenario));
        b.set_policy(&self.policy()).expect("set_policy");
        Some(b)
    }

    fn policy(&self) -> X86Policy {
        X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// The declining backend — the honest-"no" half of the exam.
// ---------------------------------------------------------------------------

/// A limited backend that forwards the common surface but has no dirty log.
struct NoDeadlineBackend(MockBackend);

impl Backend for NoDeadlineBackend {
    type A = X86;

    fn set_policy(&mut self, policy: &X86Policy) -> Result<()> {
        self.0.set_policy(policy)
    }
    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()> {
        // SAFETY: the caller upholds `map_memory`'s contract; this newtype only
        // forwards, adding no obligation.
        unsafe { self.0.map_memory(gpa, host) }
    }
    // `drain_dirty_pages` is deliberately NOT forwarded: this newtype models a
    // backend with no dirty log, so it inherits the trait's default body — and
    // the contract exam's decline check is what pins that default to
    // `Unsupported`.
    fn run(&mut self) -> Result<Exit<X86>> {
        self.0.run()
    }
    fn inject(&mut self, event: Injection) -> Result<()> {
        self.0.inject(event)
    }
    fn set_pending_irq(&mut self, id: Option<u8>) -> Result<()> {
        self.0.set_pending_irq(id)
    }
    fn take_accepted_interrupt(&mut self) -> Option<u8> {
        self.0.take_accepted_interrupt()
    }
    fn complete_read(&mut self, value: u64) -> Result<()> {
        self.0.complete_read(value)
    }
    fn complete_fault(&mut self) -> Result<()> {
        self.0.complete_fault()
    }
    fn complete_ok(&mut self) -> Result<()> {
        self.0.complete_ok()
    }
    fn complete_hypercall(&mut self, ret: u64) -> Result<()> {
        self.0.complete_hypercall(ret)
    }
    fn complete_arch(&mut self, completion: X86Completion) -> Result<()> {
        self.0.complete_arch(completion)
    }
    fn save(&self) -> Result<VcpuState> {
        self.0.save()
    }
    fn restore(&mut self, state: &VcpuState) -> Result<()> {
        self.0.restore(state)
    }
    fn exit_counts(&self) -> ExitCounts {
        self.0.exit_counts()
    }
    fn reset_exit_counts(&mut self) {
        self.0.reset_exit_counts();
    }
    fn capabilities(&self) -> MockCaps {
        self.0.capabilities()
    }
}

/// A fixture shaped like stock KVM: no dirty log, no determinism capabilities,
/// and no way to surface a hypercall or CPUID exit.
/// Everything it cannot do, it must decline **in the report** — and the
/// capability-keyed exams additionally require that it does not *claim* to trap
/// what it cannot trap.
struct LimitedFixture;

/// The limited fixture's honest capability set: it traps neither the clock nor
/// the hardware RNG.
const LIMITED_CAPS: MockCaps = Capabilities {
    name: "mock-limited",
    deterministic_rng: false,
    arch: X86Caps {
        deterministic_tsc: false,
        enforces_tsc_deadline_msr: false,
    },
};

impl BackendFixture for LimitedFixture {
    type B = NoDeadlineBackend;

    fn name(&self) -> &'static str {
        "mock-limited"
    }

    fn spawn(&mut self, scenario: Scenario) -> Option<NoDeadlineBackend> {
        match scenario {
            // Not trapped, so not claimable — the capability-keyed exam checks
            // exactly this.
            Scenario::Rdtsc | Scenario::Rdrand => None,
            // Serviced in-kernel by the substrate this fixture models.
            Scenario::Cpuid | Scenario::Hypercall => None,
            _ => {
                let mut b = MockBackend::with_capabilities(LIMITED_CAPS);
                b.extend_exits(script(scenario));
                Some(NoDeadlineBackend(b))
            }
        }
    }

    fn policy(&self) -> X86Policy {
        X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter::default(),
        }
    }

    // `dirty_pages` deliberately left at its default `None`: this backend has no
    // dirty log, which must show up as a recorded decline.
}

#[test]
fn a_limited_backend_declines_honestly_and_the_declines_are_recorded() {
    let mut fx = LimitedFixture;
    let report = run_all(&mut fx);

    assert_eq!(report.backend, "mock-limited");
    // What it CAN do, it still has to do.
    for exam in [
        "ordering/not_configured",
        "ordering/completion_grid",
        "fixpoint/save_restore_save",
        "interrupts/one_overwritable_slot",
    ] {
        assert!(report.did_run(exam), "{exam} must still run: {report:?}");
    }
    // A backend that cannot do something must say so with the documented error.
    // The exam checks the decline itself, for both capabilities this fixture
    // lacks.
    assert!(report.did_run("exactness/dirty_log_declines_loudly"));

    // Every decline is named, with its reason. This is the assertion that stops
    // a shrinking exam from reading as a passing one.
    let expect_declined = [
        Decline {
            exam: "exactness/dirty_log",
            why: DeclineReason::NoDirtyLog,
        },
        Decline {
            exam: "exactness/deterministic_tsc_traps",
            why: DeclineReason::CapabilityAbsent("deterministic_tsc"),
        },
        Decline {
            exam: "exactness/deterministic_rng_traps",
            why: DeclineReason::CapabilityAbsent("deterministic_rng"),
        },
        Decline {
            exam: "ordering/completion_grid",
            why: DeclineReason::ScenarioUnavailable(Scenario::Hypercall),
        },
        Decline {
            exam: "ordering/completion_grid",
            why: DeclineReason::ScenarioUnavailable(Scenario::Cpuid),
        },
    ];
    for decline in &expect_declined {
        assert!(
            report.declined.contains(decline),
            "{decline:?} must be recorded: {report:?}"
        );
    }
    assert_eq!(
        report.declined.len(),
        expect_declined.len(),
        "an unexpected decline means an exam stopped examining: {:?}",
        report.declined
    );
}

#[test]
fn the_exam_actually_fails_a_backend_that_breaks_the_contract() {
    // Silence the expected panic's default report so the passing run stays
    // readable; restore the hook so a later genuine panic still prints.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught = std::panic::catch_unwind(|| {
        let mut fx = BrokenFixture;
        run_all(&mut fx)
    });
    std::panic::set_hook(previous);
    assert!(
        caught.is_err(),
        "a backend that runs before set_policy must fail the ordering exam — otherwise the \
         whole suite could be passing vacuously"
    );
}
