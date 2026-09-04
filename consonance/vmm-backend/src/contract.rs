// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **`Backend` contract tests** — the shared exam every implementor of the
//! [`Backend`](crate::Backend) trait must pass (`docs/TESTING.md`).
//!
//! The trait's doc comments already *state* the contract. This module makes them
//! executable: one exam, written once, generic over the trait, run against every
//! implementor. It is behind the non-default **`contract-tests`** feature (not
//! `#[cfg(test)]`, which downstream crates cannot see) and is test-support code
//! — its exams assert and panic on failure, which is the one place in this crate
//! where that is the right shape.
//!
//! ## The three categories — and there is no fourth
//!
//! Every obligation here is **ordering**, **exactness**, or **fixpoint**
//! (`docs/ARCHITECTURE.md`, testing addendum):
//!
//! * **ordering** — operations happen in the contract's order, and an
//!   out-of-order one fails closed instead of silently mis-servicing the guest.
//! * **exactness** — quantities the engine treats as exact really are exact:
//!   dirty-page sets and repeated runs.
//! * **fixpoint** — round trips are round trips.
//!
//! "Capability honesty" is deliberately **not** a category. A
//! [`Capabilities`](crate::Capabilities) flag creates no obligation of its own;
//! it **selects which exactness exams apply** — a backend advertising a
//! deterministic clock is bound by the clock-reads-are-trapped exam, and one
//! that does not advertise it is bound to decline loudly rather than behave as
//! if it had the capability.
//!
//! ## How an implementor is examined
//!
//! Through a [`BackendFixture`]: the implementor supplies a fresh backend armed
//! for a named [`Scenario`], and the exam supplies the questions. A fixture that
//! cannot produce a scenario returns `None` — stock KVM, for instance, never
//! surfaces a hypercall exit, because the kernel services it in-kernel. Those
//! declines are recorded in [`ContractReport::declined`] and the caller asserts
//! on them. **Declines are data; a silently smaller exam is not.**
//!
//! ## Designed, not frozen
//!
//! The `Backend` trait is the ruled design, **not** a frozen surface. This suite
//! is the tripwire for future cross-vendor changes.

use crate::arch::x86::{X86, X86Completion, X86Exit, X86Policy};
use crate::backend::Backend;
use crate::error::BackendError;
use crate::exit::{CommonExit, Exit};

/// A guest situation the exam needs a backend to be in. A [`BackendFixture`]
/// translates each into whatever its substrate needs — a scripted exit queue for
/// the in-process mock, a loaded guest stub for a live KVM backend.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scenario {
    /// The guest halts immediately, repeatedly: every `run` returns
    /// [`CommonExit::Idle`] and nothing is left pending.
    Idle,
    /// The guest's first exit is a **read-style** port-I/O `IN`.
    PortIn,
    /// The guest's first exit is a **read-style** MMIO load.
    MmioLoad,
    /// The guest's first exit is a filtered MSR read.
    Rdmsr,
    /// The guest's first exit is a filtered MSR write.
    Wrmsr,
    /// The guest's first exit is a `CPUID`.
    Cpuid,
    /// The guest's first exit is the hypercall transport.
    Hypercall,
    /// The guest's first exit is `RDTSC` — only a backend advertising a
    /// deterministic timestamp counter can produce it.
    Rdtsc,
    /// The guest's first exit is `RDRAND` — only a backend advertising
    /// deterministic RNG can produce it.
    Rdrand,
}

/// What kind of completion a pending exit is waiting for. The ordering exam
/// walks (pending kind × completion method) exhaustively.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PendingKind {
    /// A read-style exit: only `complete_read` resolves it.
    Read,
    /// `Rdmsr`: `complete_read` (a value) or `complete_fault` (`deny-gp`).
    Rdmsr,
    /// `Wrmsr`: `complete_ok` (allow/drop) or `complete_fault` (`deny-gp`).
    Wrmsr,
    /// `Hypercall`: `complete_hypercall`.
    Hypercall,
    /// `Cpuid`: `complete_arch`.
    Cpuid,
}

/// The five completion methods, as data, so the ordering exam can enumerate the
/// wrong ones for a given [`PendingKind`].
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
    /// Whether this method is the (or a) correct resolution for `pending`.
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

    /// Apply this method to `backend`, discarding the value arguments (the exam
    /// cares about the *discipline*, not the payload).
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

/// What an implementor supplies so the exam can drive it.
///
/// The fixture owns whatever the substrate needs to stay alive around a backend
/// (guest memory, loaded stubs); the exam only ever holds the backend it was
/// handed, and drops it before asking for the next one.
pub trait BackendFixture {
    /// The backend under examination. Pinned to the x86 vendor: the exam's
    /// scenarios are written in x86 exits, and a second vendor gets its own
    /// vendor-shaped exam beside this one (`docs/ARCHITECTURE.md`).
    type B: Backend<A = X86>;

    /// A short, stable name for the report (`"mock"`, `"kvm-stock"`, …).
    fn name(&self) -> &'static str;

    /// A fresh backend, memory mapped and armed for `scenario`, but **not yet
    /// configured** — `set_policy` has deliberately not been called, so the
    /// ordering exam can observe the fail-closed path.
    ///
    /// `None` means this substrate cannot produce that scenario at all (a
    /// documented property of the backend, not a test skip): the exam records
    /// the decline and moves on.
    fn spawn(&mut self, scenario: Scenario) -> Option<Self::B>;

    /// The policy the exam installs with `set_policy`.
    fn policy(&self) -> X86Policy;

    /// Cause `backend` to dirty a known set of guest frames, and return those
    /// frames. `None` = this substrate cannot stage guest writes for the exam,
    /// which is recorded as a decline.
    ///
    /// The returned set is what the backend MUST report *at minimum*: the trait
    /// permits an over-report (capture-side dedup discards no-op writes) and
    /// forbids an under-report.
    fn dirty_pages(&mut self, backend: &mut Self::B) -> Option<Vec<u64>> {
        let _ = backend;
        None
    }
}

/// Why an exam did not run.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DeclineReason {
    /// The fixture cannot put its guest in this situation.
    ScenarioUnavailable(Scenario),
    /// The fixture cannot stage guest writes for the dirty-log exam.
    NoDirtyLog,
    /// The backend does not advertise the capability that selects this exam.
    /// Not a gap: the honest-decline path is itself checked.
    CapabilityAbsent(&'static str),
}

/// One exam that did not run, and why.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Decline {
    /// The exam's name.
    pub exam: &'static str,
    /// Why it did not run.
    pub why: DeclineReason,
}

/// What an exam run actually covered. The caller asserts on this — a shorter
/// exam must be visible, never silent.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ContractReport {
    /// The backend's name, from [`BackendFixture::name`].
    pub backend: &'static str,
    /// Every exam that ran, in order.
    pub ran: Vec<&'static str>,
    /// Every exam that did not run, with its reason.
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

    /// Whether `exam` ran.
    pub fn did_run(&self, exam: &'static str) -> bool {
        self.ran.contains(&exam)
    }
}

/// The scenario that produces each [`PendingKind`], and the exit the backend
/// must return for it.
fn scenario_for(kind: PendingKind) -> Scenario {
    match kind {
        PendingKind::Read => Scenario::PortIn,
        PendingKind::Rdmsr => Scenario::Rdmsr,
        PendingKind::Wrmsr => Scenario::Wrmsr,
        PendingKind::Hypercall => Scenario::Hypercall,
        PendingKind::Cpuid => Scenario::Cpuid,
    }
}

/// Assert that `exit` is the kind of exit `kind` names, so a fixture that arms
/// the wrong scenario fails here rather than distorting the grid below.
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

// ---------------------------------------------------------------------------
// ordering
// ---------------------------------------------------------------------------

/// **ordering** — the fail-closed configuration and completion discipline.
///
/// Three obligations:
///
/// 1. `run` before a successful `set_policy` is
///    [`NotConfigured`](BackendError::NotConfigured). Running on host-derived
///    CPUID/MSR defaults would leak nondeterminism, so the backend refuses.
/// 2. Resuming with an unserviced read-style exit is
///    [`PendingCompletion`](BackendError::PendingCompletion) — never a silent
///    mis-service of the guest.
/// 3. Every (pending exit kind × **wrong** completion method) cell errors. The
///    trait pins which error where: `complete_read` on a non-read-style pending
///    is [`NoPendingRead`](BackendError::NoPendingRead); `complete_fault`,
///    `complete_ok`, and `complete_arch` on a mismatched pending are
///    [`BadCompletion`](BackendError::BadCompletion). `complete_hypercall` is
///    deliberately unpinned by the trait ("Errors if none pending"), so the exam
///    requires an error and accepts either — a divergence the suite surfaces
///    rather than papers over.
pub fn ordering_exam<F: BackendFixture>(fx: &mut F, report: &mut ContractReport) {
    // (1) fail closed before set_policy.
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

    // (2) + (3) the pending/completion grid.
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

        // (2) resuming with the exit unserviced is PendingCompletion.
        assert!(
            matches!(b.run(), Err(BackendError::PendingCompletion)),
            "{kind:?}: resuming with an unserviced exit must be PendingCompletion"
        );

        // (3) every wrong completion method errors, with the pinned error where
        //     the trait pins one.
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
                // The trait leaves this one's error unpinned; require only that
                // it is loud.
                Completion::Hypercall => assert!(
                    matches!(
                        got,
                        Err(BackendError::BadCompletion | BackendError::NoPendingRead)
                    ),
                    "{kind:?} × complete_hypercall must error, got {got:?}"
                ),
            }
            // A rejected completion must NOT have cleared the pending exit — the
            // guest is still unserviced, so a resume must still fail closed.
            assert!(
                matches!(b.run(), Err(BackendError::PendingCompletion)),
                "{kind:?}: a rejected {} must leave the exit pending",
                method.name()
            );
        }

        // A correct completion resolves it, and the backend runs again.
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

/// **exactness** — the dirty-page log is sorted, deduplicated, drained on read,
/// and may over-report but never under-report.
///
/// The log is a *cost hint*, never a correctness input: a caller uses it only to
/// bound how much memory a snapshot capture re-reads. An over-report is
/// therefore harmless (capture-side dedup discards no-op writes) and an
/// under-report is silent snapshot corruption — which is why the exam asserts a
/// superset relation in one direction only.
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
        // The decline is itself under test: a
        // backend without dirty tracking must answer the documented
        // `Unsupported` so every caller takes the always-correct full-scan
        // path. Answering `Ok` with *any* set — even an empty one — would be a
        // backend claiming to have vouched for a window it never tracked, and
        // a caller that trusted it would silently corrupt a snapshot.
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

    // Retrieve-and-reset: the next drain covers exactly the span from the last
    // one, so a second drain with no writes in between is empty.
    let again = b.drain_dirty_pages().expect("second drain");
    assert!(
        again.is_empty(),
        "the log must reset on drain; a second drain replayed {again:?}"
    );
    report.ran("exactness/dirty_log");
}

/// **exactness, capability-keyed** — a backend that advertises a determinism
/// capability must actually surface the corresponding guest reads as exits.
///
/// The flag chooses the exam; it is not itself the thing under test. A backend
/// that does not advertise the capability lands in
/// [`ContractReport::declined`] with [`DeclineReason::CapabilityAbsent`] — an
/// honest, recorded "no", never a silent pass.
pub fn capability_keyed_exactness_exam<F: BackendFixture>(fx: &mut F, report: &mut ContractReport) {
    // The capability flags are read from a backend, so spawn one to ask.
    let Some(probe) = fx.spawn(Scenario::Idle) else {
        report.decline(
            "exactness/capability_keyed",
            DeclineReason::ScenarioUnavailable(Scenario::Idle),
        );
        return;
    };
    let caps = probe.capabilities();
    drop(probe);

    // Deterministic timestamp counter -> the guest's clock reads must exit.
    if caps.arch.deterministic_tsc {
        let mut b = fx
            .spawn(Scenario::Rdtsc)
            .expect("a backend advertising deterministic_tsc must be able to trap RDTSC");
        b.set_policy(&fx.policy()).expect("set_policy");
        let exit = b.run().expect("run to RDTSC");
        assert_eq!(
            exit,
            Exit::Arch(X86Exit::Rdtsc),
            "deterministic_tsc is advertised, so the guest's clock read must surface as an \
             exit the VMM resolves to V-time"
        );
        b.complete_read(0x1234).expect("resolve the clock read");
        report.ran("exactness/deterministic_tsc_traps");
    } else {
        report.decline(
            "exactness/deterministic_tsc_traps",
            DeclineReason::CapabilityAbsent("deterministic_tsc"),
        );
        // The honest-decline path: it must not surface the exit it cannot trap.
        assert!(
            fx.spawn(Scenario::Rdtsc).is_none(),
            "a backend that does not advertise deterministic_tsc must not claim it can trap \
             the guest's clock reads"
        );
    }

    // Deterministic RNG -> the guest's hardware-RNG reads must exit.
    if caps.deterministic_rng {
        let mut b = fx
            .spawn(Scenario::Rdrand)
            .expect("a backend advertising deterministic_rng must be able to trap RDRAND");
        b.set_policy(&fx.policy()).expect("set_policy");
        match b.run().expect("run to RDRAND") {
            Exit::Arch(X86Exit::Rdrand { .. }) => {}
            other => panic!(
                "deterministic_rng is advertised, so the guest's RNG read must surface as an \
                 exit resolvable to the seeded stream; got {other:?}"
            ),
        }
        b.complete_read(0xABCD).expect("resolve the RNG read");
        report.ran("exactness/deterministic_rng_traps");
    } else {
        report.decline(
            "exactness/deterministic_rng_traps",
            DeclineReason::CapabilityAbsent("deterministic_rng"),
        );
        assert!(
            fx.spawn(Scenario::Rdrand).is_none(),
            "a backend that does not advertise deterministic_rng must not claim it can trap \
             the guest's RNG reads"
        );
    }
}

// ---------------------------------------------------------------------------
// fixpoint
// ---------------------------------------------------------------------------

/// **fixpoint** — `save → restore → save` is the identity, and a malformed blob
/// is an error rather than a panic.
///
/// The malformed-blob arm is only meaningful for a backend that can reject one;
/// a substrate with no host to validate against (the in-process mock) accepts
/// any well-typed `VcpuState` by construction, so the exam requires only that it
/// does not panic.
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

    // A malformed blob is `InvalidState`, never a panic. Corrupt a state the
    // backend itself produced so the blob is well-typed but internally
    // inconsistent; either arm is contract-conformant, a panic is not.
    let mut malformed = first.clone();
    malformed.sregs.cs.limit = u32::MAX;
    malformed.sregs.cs.selector = u16::MAX;
    match b.restore(&malformed) {
        Ok(()) | Err(BackendError::InvalidState) => {}
        Err(other) => panic!("a malformed VcpuState must be InvalidState, got {other:?}"),
    }
    report.ran("fixpoint/save_restore_save");
}

// ---------------------------------------------------------------------------
// the interrupt delivery contract
// ---------------------------------------------------------------------------

/// The **interrupt delivery contract** — `set_pending_irq` is one overwritable
/// slot, never a queue, and `take_accepted_interrupt` reports only interrupts
/// actually issued into the guest.
///
/// This is the contract most easily got subtly wrong, because a queue "works"
/// until the guest raises its priority threshold: the VMM owns the userspace
/// interrupt fabric, whose pending-register file *is* the multi-interrupt queue,
/// and it re-arbitrates at every entry. A backend that queued identities would
/// deliver a stale one.
pub fn interrupt_delivery_exam<F: BackendFixture>(fx: &mut F, report: &mut ContractReport) {
    // Staged is not accepted: an identity set but not yet entered on must not be
    // reported as delivered.
    let Some(mut b) = fx.spawn(Scenario::Idle) else {
        report.decline(
            "interrupts/one_overwritable_slot",
            DeclineReason::ScenarioUnavailable(Scenario::Idle),
        );
        return;
    };
    b.set_policy(&fx.policy()).expect("set_policy");
    if let Err(BackendError::Unsupported { .. }) = b.set_pending_irq(Some(0x30)) {
        // A backend with no delivery fabric (the ruled arm64 skeleton) declines
        // the whole contract; record that rather than pretending to test it.
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

    // One slot, not a queue: a second set overwrites the first, and exactly one
    // identity is ever accepted.
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

    // `None` clears the slot (and disarms the interrupt window).
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

// ---------------------------------------------------------------------------
// the whole exam
// ---------------------------------------------------------------------------

/// Run every exam against `fx` and return what it covered.
///
/// The report is the deliverable, not the absence of a panic: a caller asserts
/// both that the exams it expects ran and that the declines it sees are the ones
/// the backend documents.
pub fn run_all<F: BackendFixture>(fx: &mut F) -> ContractReport {
    let mut report = ContractReport::new(fx.name());
    ordering_exam(fx, &mut report);
    dirty_log_exactness_exam(fx, &mut report);
    capability_keyed_exactness_exam(fx, &mut report);
    fixpoint_exam(fx, &mut report);
    interrupt_delivery_exam(fx, &mut report);
    report
}
