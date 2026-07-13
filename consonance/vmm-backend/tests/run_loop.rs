// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 (`MockBackend` drives the run-loop contract) and gate 2 (the run-loop
//! / completion proptest). Both exercise the portable trait with no `/dev/kvm`.
//!
//! The mock is behind the non-default `mock` feature; this whole file compiles to
//! nothing without it (the gates run `--all-features`).
#![cfg(feature = "mock")]

use std::collections::BTreeMap;

use proptest::prelude::*;
use vmm_backend::{
    Backend, BackendError, Capabilities, Completion, CpuidModel, Injection, Exit, ExitReason, Gpa,
    HypercallRegs, MockBackend, MsrFilter, VcpuState, Moment,
};

/// Proptest case count: full per the convention natively, cut to 16 under Miri
/// (the interpreter is ~10–100× slower) with failure-persistence disabled
/// (its default path resolution uses `getcwd`, which Miri's fs isolation
/// rejects). Mirrors `vmcall-transport`'s `config` helper.
fn cases(native: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { native });
    if cfg!(miri) {
        cfg.failure_persistence = None;
    }
    cfg
}

/// A configured mock (both config calls landed, so `run` is past `NotConfigured`).
fn configured() -> MockBackend {
    let mut m = MockBackend::new();
    m.set_cpuid(&CpuidModel::default()).expect("set_cpuid");
    m.set_msr_filter(&MsrFilter::default())
        .expect("set_msr_filter");
    m
}

/// Apply the matching completion for a just-returned exit (the discipline a live
/// VMM must follow before the next `run`).
fn complete_correctly(m: &mut MockBackend, exit: &Exit) -> Result<(), BackendError> {
    match exit {
        Exit::Io { write: None, .. }
        | Exit::Mmio { write: None, .. }
        | Exit::Rdtsc
        | Exit::Rdtscp
        | Exit::Rdrand { .. }
        | Exit::Rdseed { .. }
        | Exit::Rdmsr { .. } => m.complete_read(0),
        Exit::Wrmsr { .. } => m.complete_ok(),
        Exit::Hypercall(_) => m.complete_hypercall(0),
        Exit::Cpuid { .. } => m.complete_cpuid(0, 0, 0, 0),
        // Write-style / terminal exits need no completion.
        Exit::Io { write: Some(_), .. }
        | Exit::Mmio { write: Some(_), .. }
        | Exit::Hlt
        | Exit::Shutdown
        | Exit::Deadline { .. } => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Gate 1 — unit tests covering the contract corners.
// ---------------------------------------------------------------------------

#[test]
fn run_before_configured_fails_closed() {
    let mut m = MockBackend::new();
    m.push_exit(Exit::Hlt);
    assert!(matches!(m.run(), Err(BackendError::NotConfigured)));

    // Only one of the two config calls is not enough — still fail closed.
    m.set_cpuid(&CpuidModel::default()).unwrap();
    assert!(!m.is_configured());
    assert!(matches!(m.run(), Err(BackendError::NotConfigured)));

    m.set_msr_filter(&MsrFilter::default()).unwrap();
    assert!(m.is_configured());
    assert_eq!(m.run().unwrap(), Exit::Hlt);
}

#[test]
fn missed_completion_is_pending_completion() {
    let mut m = configured();
    m.extend_exits([
        Exit::Io {
            port: 0x80,
            size: 1,
            write: None,
        },
        Exit::Hlt,
    ]);

    let e = m.run().unwrap();
    assert_eq!(
        e,
        Exit::Io {
            port: 0x80,
            size: 1,
            write: None
        }
    );
    assert!(m.has_pending());
    // Resuming with the read un-serviced fails closed.
    assert!(matches!(m.run(), Err(BackendError::PendingCompletion)));
    // Service it, then resume.
    m.complete_read(0x55).unwrap();
    assert!(!m.has_pending());
    assert_eq!(m.run().unwrap(), Exit::Hlt);
    assert_eq!(m.completions(), &[Completion::Read(0x55)]);
}

#[test]
fn out_exit_needs_no_completion() {
    let mut m = configured();
    m.extend_exits([
        Exit::Io {
            port: 0x80,
            size: 1,
            write: Some(0xAB),
        },
        Exit::Mmio {
            gpa: Gpa(0xFEE0_0000),
            size: 4,
            write: Some(0x1),
        },
        Exit::Hlt,
    ]);
    assert!(matches!(m.run().unwrap(), Exit::Io { write: Some(_), .. }));
    assert!(!m.has_pending());
    assert!(matches!(
        m.run().unwrap(),
        Exit::Mmio { write: Some(_), .. }
    ));
    assert!(!m.has_pending());
    assert_eq!(m.run().unwrap(), Exit::Hlt);
}

#[test]
fn complete_read_without_pending_errors() {
    let mut m = configured();
    m.push_exit(Exit::Hlt);
    // Nothing pending yet.
    assert!(matches!(
        m.complete_read(0),
        Err(BackendError::NoPendingRead)
    ));
    // After a no-completion exit, still nothing read-style pending.
    assert_eq!(m.run().unwrap(), Exit::Hlt);
    assert!(matches!(
        m.complete_read(0),
        Err(BackendError::NoPendingRead)
    ));
}

#[test]
fn rdmsr_accepts_read_or_fault() {
    // value path
    let mut m = configured();
    m.push_exit(Exit::Rdmsr { index: 0x1B });
    m.run().unwrap();
    m.complete_read(0xDEAD).unwrap();
    assert_eq!(m.completions(), &[Completion::Read(0xDEAD)]);

    // deny-gp path
    let mut m = configured();
    m.push_exit(Exit::Rdmsr { index: 0x1B });
    m.run().unwrap();
    m.complete_fault().unwrap();
    assert_eq!(m.completions(), &[Completion::Fault]);
}

#[test]
fn wrmsr_accepts_ok_or_fault_but_not_read() {
    // A Wrmsr is not read-style: complete_read must be rejected.
    let mut m = configured();
    m.push_exit(Exit::Wrmsr {
        index: 0x6E0,
        value: 1,
    });
    m.run().unwrap();
    assert!(matches!(
        m.complete_read(0),
        Err(BackendError::NoPendingRead)
    ));
    assert!(matches!(
        m.complete_cpuid(0, 0, 0, 0),
        Err(BackendError::BadCompletion)
    ));
    m.complete_ok().unwrap();
    assert_eq!(m.completions(), &[Completion::Ok]);

    // deny-gp on a write.
    let mut m = configured();
    m.push_exit(Exit::Wrmsr {
        index: 0x6E0,
        value: 1,
    });
    m.run().unwrap();
    m.complete_fault().unwrap();
    assert_eq!(m.completions(), &[Completion::Fault]);
}

#[test]
fn hypercall_and_cpuid_completions_are_typed() {
    let mut m = configured();
    m.push_exit(Exit::Hypercall(HypercallRegs::default()));
    m.run().unwrap();
    assert!(matches!(m.complete_ok(), Err(BackendError::BadCompletion)));
    m.complete_hypercall(48).unwrap();
    assert_eq!(m.completions(), &[Completion::Hypercall(48)]);

    // `complete_hypercall` distinguishes nothing-pending (NoPendingRead) from a
    // different pending exit (BadCompletion) — both arms are load-bearing.
    let mut m = configured();
    assert!(matches!(
        m.complete_hypercall(0),
        Err(BackendError::NoPendingRead)
    ));
    m.push_exit(Exit::Rdmsr { index: 0x1B });
    m.run().unwrap();
    assert!(matches!(
        m.complete_hypercall(0),
        Err(BackendError::BadCompletion)
    ));

    let mut m = configured();
    m.push_exit(Exit::Cpuid {
        leaf: 1,
        subleaf: 0,
    });
    m.run().unwrap();
    assert!(matches!(
        m.complete_fault(),
        Err(BackendError::BadCompletion)
    ));
    m.complete_cpuid(0xA, 0xB, 0xC, 0xD).unwrap();
    assert_eq!(
        m.completions(),
        &[Completion::Cpuid {
            eax: 0xA,
            ebx: 0xB,
            ecx: 0xC,
            edx: 0xD
        }]
    );
}

#[test]
fn counters_increment_per_reason_and_reset() {
    let mut m = configured();
    m.extend_exits([
        Exit::Io {
            port: 1,
            size: 1,
            write: Some(0),
        },
        Exit::Io {
            port: 2,
            size: 1,
            write: Some(0),
        },
        Exit::Hlt,
    ]);
    m.run().unwrap();
    m.run().unwrap();
    m.run().unwrap();
    let c = m.exit_counts();
    assert_eq!(c.io, 2);
    assert_eq!(c.hlt, 1);
    assert_eq!(c.total(), 3);

    m.reset_exit_counts();
    assert_eq!(m.exit_counts().total(), 0);
}

#[test]
fn run_until_returns_deadline_with_requested_value() {
    let mut m = configured();
    m.extend_exits([Exit::Deadline { reached: Moment(0) }]);
    let e = m.run_until(Moment(4096)).unwrap();
    assert_eq!(
        e,
        Exit::Deadline {
            reached: Moment(4096)
        }
    );
    assert_eq!(m.exit_counts().deadline, 1);
    assert!(!m.has_pending());
}

#[test]
fn inject_records_events() {
    let mut m = configured();
    m.inject(Injection::Interrupt { vector: 0x20 }).unwrap();
    m.inject(Injection::Nmi).unwrap();
    assert_eq!(
        m.injected(),
        &[Injection::Interrupt { vector: 0x20 }, Injection::Nmi]
    );
}

#[test]
fn set_pending_irq_overwrites_single_slot() {
    // The VMM re-arbitrates from the LAPIC each entry and overwrites the single
    // pending slot: the LATEST `set_pending_irq` wins (the multi-IRQ "queue" is the
    // LAPIC IRR, above the trait). So a re-arbitrated higher-priority vector replaces
    // the earlier one, and `None` retracts a now-stale vector.
    let mut m = configured();
    m.set_pending_irq(Some(0x40)).unwrap();
    m.set_pending_irq(Some(0x50)).unwrap(); // re-arbitration: 0x50 replaces 0x40
    assert_eq!(m.pending_irq(), Some(0x50));
    m.push_exit(Exit::Hlt);
    assert_eq!(m.run().expect("run"), Exit::Hlt);
    // Only the last-set vector is accepted; 0x40 was never injected.
    assert_eq!(m.take_accepted_interrupt(), Some(0x50));
    assert_eq!(m.take_accepted_interrupt(), None);

    // Re-arbitrating to `None` retracts a stale vector (the P2 fix): it is not
    // accepted, even though the backend would otherwise be injectable.
    let mut m = configured();
    m.set_pending_irq(Some(0x40)).unwrap();
    m.set_pending_irq(None).unwrap(); // TPR raised / vector no longer deliverable
    assert_eq!(m.pending_irq(), None);
    m.push_exit(Exit::Hlt);
    assert_eq!(m.run().expect("run"), Exit::Hlt);
    assert_eq!(
        m.take_accepted_interrupt(),
        None,
        "a retracted (re-arbitrated-away) vector is never injected"
    );
}

#[test]
fn deferred_accept_holds_irq_pending() {
    // With acceptance deferred (the interrupt-window wait), the pending IRQ is held —
    // not reported accepted — until acceptance is re-enabled.
    let mut m = configured();
    m.set_defer_accept(true);
    m.set_pending_irq(Some(0x40)).unwrap();
    m.push_exit(Exit::Hlt);
    assert_eq!(m.run().expect("run"), Exit::Hlt);
    assert_eq!(
        m.take_accepted_interrupt(),
        None,
        "held pending while deferred"
    );
    assert_eq!(m.pending_irq(), Some(0x40), "still pending (un-accepted)");
    // Re-enable acceptance; the next entry accepts it.
    m.set_defer_accept(false);
    m.push_exit(Exit::Hlt);
    assert_eq!(m.run().expect("run"), Exit::Hlt);
    assert_eq!(m.take_accepted_interrupt(), Some(0x40));
}

#[test]
fn mock_observability_and_config_getters() {
    // with_capabilities overrides the reported caps.
    let caps = Capabilities {
        name: "test-mock",
        deterministic_tsc: true,
        deterministic_rng: false,
        enforces_tsc_deadline_msr: true,
    };
    assert_eq!(MockBackend::with_capabilities(caps).capabilities(), caps);

    let mut m = MockBackend::new();
    assert!(m.installed_cpuid().is_none());
    assert!(m.installed_msr_filter().is_none());
    assert!(!m.is_configured());

    let model = CpuidModel::default();
    let filter = MsrFilter::default();
    m.set_cpuid(&model).unwrap();
    m.set_msr_filter(&filter).unwrap();
    assert_eq!(m.installed_cpuid(), Some(&model));
    assert_eq!(m.installed_msr_filter(), Some(&filter));
    assert!(m.is_configured());

    // map_memory records (gpa, len); set_state feeds save().
    let mut mem = [0u8; 8192];
    // SAFETY: the mock does not dereference `mem`; it only records the region.
    unsafe { m.map_memory(Gpa(0x1000), &mut mem) }.unwrap();
    assert_eq!(m.regions(), &[(Gpa(0x1000), 8192)]);

    let mut st = VcpuState::default();
    st.regs.rax = 0x1234;
    m.set_state(st.clone());
    assert_eq!(m.save().unwrap(), st);
}

#[test]
fn mock_map_memory_validation_errors() {
    let mut m = MockBackend::new();
    // zero-length / mis-aligned gpa / mis-aligned length all reject.
    // SAFETY: the mock never dereferences the slice; these all error before use.
    assert!(unsafe { m.map_memory(Gpa(0), &mut []) }.is_err());
    assert!(unsafe { m.map_memory(Gpa(1), &mut [0u8; 4096]) }.is_err());
    assert!(unsafe { m.map_memory(Gpa(0), &mut [0u8; 100]) }.is_err());

    // overlapping maps reject.
    let mut a = [0u8; 4096];
    let mut b = [0u8; 4096];
    unsafe { m.map_memory(Gpa(0), &mut a) }.unwrap();
    assert!(matches!(
        unsafe { m.map_memory(Gpa(0), &mut b) },
        Err(BackendError::Memory(_))
    ));
}

/// Task 95 M2.1: the trait default declines (`Unsupported` — callers full-scan),
/// and `Box<dyn Backend>` **forwards** the harvest to the inner impl instead of
/// re-answering the default — the shadowing landmine the explicit blanket
/// forward exists to disarm. The scripted set comes back sorted + deduplicated.
#[test]
fn harvest_default_declines_and_box_forwards_to_the_inner_impl() {
    let mut plain = MockBackend::new();
    assert!(matches!(
        plain.harvest_dirty_gfns(),
        Err(BackendError::Unsupported { .. })
    ));

    let mut m = MockBackend::new();
    m.push_dirty_gfns(vec![9, 2, 2]);
    let mut boxed: Box<dyn Backend> = Box::new(m);
    assert_eq!(boxed.harvest_dirty_gfns().unwrap(), vec![2, 9]);
    // Drained: the next harvest window is empty, not a replay of the last.
    assert_eq!(boxed.harvest_dirty_gfns().unwrap(), Vec::<u64>::new());
}

// ---------------------------------------------------------------------------
// Gate 2 — the core run-loop / completion proptest (≥256 cases).
// ---------------------------------------------------------------------------

/// An arbitrary `Exit` spanning every variant.
fn arb_exit() -> impl Strategy<Value = Exit> {
    prop_oneof![
        (any::<u16>(), 1u8..=4, any::<Option<u32>>()).prop_map(|(port, size, write)| Exit::Io {
            port,
            size,
            write
        }),
        (any::<u64>(), 1u8..=8, any::<Option<u64>>()).prop_map(|(g, size, write)| Exit::Mmio {
            gpa: Gpa(g),
            size,
            write
        }),
        any::<u32>().prop_map(|index| Exit::Rdmsr { index }),
        (any::<u32>(), any::<u64>()).prop_map(|(index, value)| Exit::Wrmsr { index, value }),
        any::<[u64; 4]>().prop_map(|r| Exit::Hypercall(HypercallRegs {
            rax: r[0],
            rbx: r[1],
            rcx: r[2],
            rdx: r[3],
        })),
        (any::<u32>(), any::<u32>()).prop_map(|(leaf, subleaf)| Exit::Cpuid { leaf, subleaf }),
        Just(Exit::Rdtsc),
        Just(Exit::Rdtscp),
        (2u8..=8).prop_map(|width| Exit::Rdrand { width }),
        (2u8..=8).prop_map(|width| Exit::Rdseed { width }),
        Just(Exit::Hlt),
        Just(Exit::Shutdown),
        any::<u64>().prop_map(|v| Exit::Deadline { reached: Moment(v) }),
    ]
}

proptest! {
    #![proptest_config(cases(256))]

    /// Driving a scripted sequence with correct completions: every `run`
    /// succeeds, the returned exit equals the scripted one, and the final
    /// `exit_counts()` matches the reason histogram exactly.
    #[test]
    fn counts_match_histogram(script in proptest::collection::vec(arb_exit(), 0..40)) {
        let mut m = configured();
        m.extend_exits(script.clone());

        let mut expected: BTreeMap<ExitReason, u64> = BTreeMap::new();
        for scripted in &script {
            let got = m.run().expect("run");
            // run_until-only `Deadline.reached` is preserved verbatim by `run`.
            prop_assert_eq!(&got, scripted);
            complete_correctly(&mut m, &got).expect("complete");
            *expected.entry(got.reason()).or_default() += 1;
        }

        let counts = m.exit_counts();
        for (reason, n) in counts.entries() {
            prop_assert_eq!(n, expected.get(&reason).copied().unwrap_or(0));
        }
        prop_assert_eq!(counts.total(), script.len() as u64);
    }

    /// Completion discipline is enforced exactly: skipping a needed completion
    /// makes the next `run` fail closed with `PendingCompletion`; a no-completion
    /// exit lets the next `run` proceed. Nothing in any branch panics.
    #[test]
    fn discipline_is_enforced(script in proptest::collection::vec(arb_exit(), 1..40)) {
        let mut m = configured();
        m.extend_exits(script.clone());
        // One extra exit so there is always a "next" run to probe.
        m.push_exit(Exit::Shutdown);

        for scripted in &script {
            let got = m.run().expect("run");
            let needs_completion = m.has_pending();
            // The pending flag is exactly "this exit needs a completion".
            let is_read_style = matches!(scripted,
                Exit::Io { write: None, .. } | Exit::Mmio { write: None, .. }
                | Exit::Rdmsr { .. } | Exit::Wrmsr { .. } | Exit::Hypercall(_)
                | Exit::Cpuid { .. } | Exit::Rdtsc | Exit::Rdtscp
                | Exit::Rdrand { .. } | Exit::Rdseed { .. });
            prop_assert_eq!(needs_completion, is_read_style);

            if needs_completion {
                // Resuming without completing fails closed...
                prop_assert!(matches!(m.run(), Err(BackendError::PendingCompletion)));
                // ...and a correct completion clears it.
                complete_correctly(&mut m, &got).expect("complete");
                prop_assert!(!m.has_pending());
            }
        }
        prop_assert_eq!(m.run().expect("trailing run"), Exit::Shutdown);
    }
}
