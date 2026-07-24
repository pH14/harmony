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
    Backend, BackendError, Capabilities, CommonExit, Completion, CpuidModel, Exit, ExitReason, Gpa,
    HypercallFrame, Injection, MockBackend, Moment, MsrFilter, VcpuState, X86, X86Caps,
    X86Completion, X86Exit, X86Policy,
};

/// Proptest case count: full per the convention natively, cut to 16 under Miri
/// (the interpreter is ~10–100× slower) with failure-persistence disabled
/// (its default path resolution uses `getcwd`, which Miri's fs isolation
/// rejects). Mirrors `hypercall-doorbell`'s `config` helper.
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
    m.set_policy(&X86Policy {
        cpuid: CpuidModel::default(),
        msr_filter: MsrFilter::default(),
    })
    .expect("set_policy");
    m
}

/// Apply the matching completion for a just-returned exit (the discipline a live
/// VMM must follow before the next `run`).
fn complete_correctly(m: &mut MockBackend, exit: &Exit<X86>) -> Result<(), BackendError> {
    match exit {
        Exit::Arch(X86Exit::Io { write: None, .. })
        | Exit::Common(CommonExit::Mmio { write: None, .. })
        | Exit::Arch(X86Exit::Rdtsc)
        | Exit::Arch(X86Exit::Rdtscp)
        | Exit::Arch(X86Exit::Rdrand { .. })
        | Exit::Arch(X86Exit::Rdseed { .. })
        | Exit::Arch(X86Exit::Rdmsr { .. }) => m.complete_read(0),
        Exit::Arch(X86Exit::Wrmsr { .. }) => m.complete_ok(),
        Exit::Common(CommonExit::Hypercall(_)) => m.complete_hypercall(0),
        Exit::Arch(X86Exit::Cpuid { .. }) => m.complete_arch(X86Completion::Cpuid {
            eax: 0,
            ebx: 0,
            ecx: 0,
            edx: 0,
        }),
        // Write-style / terminal exits need no completion.
        Exit::Arch(X86Exit::Io { write: Some(_), .. })
        | Exit::Common(CommonExit::Mmio { write: Some(_), .. })
        | Exit::Common(CommonExit::Idle)
        | Exit::Common(CommonExit::Shutdown)
        | Exit::Common(CommonExit::Deadline { .. }) => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Gate 1 — unit tests covering the contract corners.
// ---------------------------------------------------------------------------

#[test]
fn run_before_configured_fails_closed() {
    let mut m = MockBackend::new();
    m.push_exit(Exit::Common(CommonExit::Idle));
    assert!(!m.is_configured());
    assert!(matches!(m.run(), Err(BackendError::NotConfigured)));

    m.set_policy(&X86Policy::default()).unwrap();
    assert!(m.is_configured());
    assert_eq!(m.run().unwrap(), Exit::Common(CommonExit::Idle));
}

#[test]
fn missed_completion_is_pending_completion() {
    let mut m = configured();
    m.extend_exits([
        Exit::Arch(X86Exit::Io {
            port: 0x80,
            size: 1,
            write: None,
        }),
        Exit::Common(CommonExit::Idle),
    ]);

    let e = m.run().unwrap();
    assert_eq!(
        e,
        Exit::Arch(X86Exit::Io {
            port: 0x80,
            size: 1,
            write: None
        })
    );
    assert!(m.has_pending());
    // Resuming with the read un-serviced fails closed.
    assert!(matches!(m.run(), Err(BackendError::PendingCompletion)));
    // Service it, then resume.
    m.complete_read(0x55).unwrap();
    assert!(!m.has_pending());
    assert_eq!(m.run().unwrap(), Exit::Common(CommonExit::Idle));
    assert_eq!(m.completions(), &[Completion::Read(0x55)]);
}

#[test]
fn out_exit_needs_no_completion() {
    let mut m = configured();
    m.extend_exits([
        Exit::Arch(X86Exit::Io {
            port: 0x80,
            size: 1,
            write: Some(0xAB),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0xFEE0_0000),
            size: 4,
            write: Some(0x1),
        }),
        Exit::Common(CommonExit::Idle),
    ]);
    assert!(matches!(
        m.run().unwrap(),
        Exit::Arch(X86Exit::Io { write: Some(_), .. })
    ));
    assert!(!m.has_pending());
    assert!(matches!(
        m.run().unwrap(),
        Exit::Common(CommonExit::Mmio { write: Some(_), .. })
    ));
    assert!(!m.has_pending());
    assert_eq!(m.run().unwrap(), Exit::Common(CommonExit::Idle));
}

#[test]
fn complete_read_without_pending_errors() {
    let mut m = configured();
    m.push_exit(Exit::Common(CommonExit::Idle));
    // Nothing pending yet.
    assert!(matches!(
        m.complete_read(0),
        Err(BackendError::NoPendingRead)
    ));
    // After a no-completion exit, still nothing read-style pending.
    assert_eq!(m.run().unwrap(), Exit::Common(CommonExit::Idle));
    assert!(matches!(
        m.complete_read(0),
        Err(BackendError::NoPendingRead)
    ));
}

#[test]
fn rdmsr_accepts_read_or_fault() {
    // value path
    let mut m = configured();
    m.push_exit(Exit::Arch(X86Exit::Rdmsr { index: 0x1B }));
    m.run().unwrap();
    m.complete_read(0xDEAD).unwrap();
    assert_eq!(m.completions(), &[Completion::Read(0xDEAD)]);

    // deny-gp path
    let mut m = configured();
    m.push_exit(Exit::Arch(X86Exit::Rdmsr { index: 0x1B }));
    m.run().unwrap();
    m.complete_fault().unwrap();
    assert_eq!(m.completions(), &[Completion::Fault]);
}

#[test]
fn wrmsr_accepts_ok_or_fault_but_not_read() {
    // A Wrmsr is not read-style: complete_read must be rejected.
    let mut m = configured();
    m.push_exit(Exit::Arch(X86Exit::Wrmsr {
        index: 0x6E0,
        value: 1,
    }));
    m.run().unwrap();
    assert!(matches!(
        m.complete_read(0),
        Err(BackendError::NoPendingRead)
    ));
    assert!(matches!(
        m.complete_arch(X86Completion::Cpuid {
            eax: 0,
            ebx: 0,
            ecx: 0,
            edx: 0,
        }),
        Err(BackendError::BadCompletion)
    ));
    m.complete_ok().unwrap();
    assert_eq!(m.completions(), &[Completion::Ok]);

    // deny-gp on a write.
    let mut m = configured();
    m.push_exit(Exit::Arch(X86Exit::Wrmsr {
        index: 0x6E0,
        value: 1,
    }));
    m.run().unwrap();
    m.complete_fault().unwrap();
    assert_eq!(m.completions(), &[Completion::Fault]);
}

#[test]
fn hypercall_and_cpuid_completions_are_typed() {
    let mut m = configured();
    m.push_exit(Exit::Common(CommonExit::Hypercall(
        HypercallFrame::default(),
    )));
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
    m.push_exit(Exit::Arch(X86Exit::Rdmsr { index: 0x1B }));
    m.run().unwrap();
    assert!(matches!(
        m.complete_hypercall(0),
        Err(BackendError::BadCompletion)
    ));

    let mut m = configured();
    m.push_exit(Exit::Arch(X86Exit::Cpuid {
        leaf: 1,
        subleaf: 0,
    }));
    m.run().unwrap();
    assert!(matches!(
        m.complete_fault(),
        Err(BackendError::BadCompletion)
    ));
    m.complete_arch(X86Completion::Cpuid {
        eax: 0xA,
        ebx: 0xB,
        ecx: 0xC,
        edx: 0xD,
    })
    .unwrap();
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
        Exit::Arch(X86Exit::Io {
            port: 1,
            size: 1,
            write: Some(0),
        }),
        Exit::Arch(X86Exit::Io {
            port: 2,
            size: 1,
            write: Some(0),
        }),
        Exit::Common(CommonExit::Idle),
    ]);
    m.run().unwrap();
    m.run().unwrap();
    m.run().unwrap();
    let c = m.exit_counts();
    assert_eq!(c.io, 2);
    assert_eq!(c.idle, 1);
    assert_eq!(c.total(), 3);

    m.reset_exit_counts();
    assert_eq!(m.exit_counts().total(), 0);
}

#[test]
fn run_until_folds_an_at_or_before_reached_up_to_the_deadline() {
    // Required regression 1 (task 156, hm-j16h): a script entry with
    // `reached < deadline` lands EXACTLY at the deadline. `run_until` folds the
    // scripted `reached` with the requested deadline via max, so `reached < deadline`
    // is unrepresentable — the frozen `CommonExit::Deadline` invariant
    // (`reached >= deadline`) holds by construction. Here `reached: 0 < 4096`.
    let mut m = configured();
    m.extend_exits([Exit::Common(CommonExit::Deadline { reached: Moment(0) })]);
    let e = m.run_until(Moment(4096)).unwrap();
    assert_eq!(
        e,
        Exit::Common(CommonExit::Deadline {
            reached: Moment(4096)
        }),
        "an at-or-before scripted reached (0) clamps up to exactly the deadline (4096)"
    );
    assert_eq!(m.exit_counts().deadline, 1);
    assert!(!m.has_pending());
}

#[test]
fn run_until_folds_a_late_reached_past_the_requested_deadline() {
    // Required regression 2 (task 156, hm-j16h): a genuinely late script entry
    // (`reached > deadline`) still lands LATE, at its scripted boundary — the guest
    // free-ran to the next natural boundary, the box @3e7 overshoot the exact-count
    // seam could not clamp. Lateness now rides on the script entry's `reached`
    // itself; `run_until` folds via max, so `reached > deadline` passes through
    // unchanged. (Formerly the `push_late_landing` queue; migrated to the
    // script-entry form.)
    let mut m = configured();
    m.extend_exits([Exit::Common(CommonExit::Deadline {
        reached: Moment(5000),
    })]);
    let e = m.run_until(Moment(4096)).unwrap();
    assert_eq!(
        e,
        Exit::Common(CommonExit::Deadline {
            reached: Moment(5000)
        }),
        "the leg lands at the scripted boundary 5000, PAST the requested 4096"
    );
    assert_eq!(m.exit_counts().deadline, 1);
    assert!(!m.has_pending());
}

#[test]
fn each_run_until_leg_folds_its_own_scripted_reached_independently() {
    // Lateness rides on the script entry, so successive legs fold independently —
    // there is no shared queue to misalign (the F3 hazard the fold removes). A late
    // entry lands late; an at-or-before entry clamps up to its own deadline.
    // Determinism: explicit, ordered test inputs — no clock, no randomness.
    // (Formerly `late_landings_are_a_fifo_queue_that_drains_to_exact`.)
    let mut m = configured();
    m.extend_exits([
        Exit::Common(CommonExit::Deadline {
            reached: Moment(11),
        }),
        Exit::Common(CommonExit::Deadline {
            reached: Moment(22),
        }),
        Exit::Common(CommonExit::Deadline { reached: Moment(0) }),
    ]);
    assert_eq!(
        m.run_until(Moment(1)).unwrap(),
        Exit::Common(CommonExit::Deadline {
            reached: Moment(11)
        }),
        "reached 11 > deadline 1 → lands late at 11"
    );
    assert_eq!(
        m.run_until(Moment(2)).unwrap(),
        Exit::Common(CommonExit::Deadline {
            reached: Moment(22)
        }),
        "reached 22 > deadline 2 → lands late at 22"
    );
    assert_eq!(
        m.run_until(Moment(3)).unwrap(),
        Exit::Common(CommonExit::Deadline { reached: Moment(3) }),
        "reached 0 < deadline 3 → clamps up to exactly 3"
    );
}

#[test]
fn run_passes_deadline_verbatim_while_run_until_folds() {
    // The fold lives in `run_until` only. Plain `run` (no deadline) returns a
    // scripted `Deadline` verbatim — no fold — so an arbitrary `reached` is
    // preserved (this is why the arbitrary-reached proptest `counts_match_histogram`
    // exercises `run`, not `run_until`). `run_until` instead folds the scripted
    // `reached` with the requested deadline via max.
    // (Formerly `late_landing_only_affects_run_until_not_run`.)
    let mut m = configured();
    m.extend_exits([
        Exit::Common(CommonExit::Deadline { reached: Moment(7) }),
        Exit::Common(CommonExit::Deadline {
            reached: Moment(99),
        }),
    ]);
    // `run` passes the scripted Deadline verbatim (reached:7 preserved — even though
    // 7 could be below a hypothetical deadline; `run` never folds).
    assert_eq!(
        m.run().unwrap(),
        Exit::Common(CommonExit::Deadline { reached: Moment(7) }),
        "run passes the scripted Deadline verbatim, never folding"
    );
    // `run_until` folds the next entry: max(99, 4) = 99 (a genuinely late entry
    // lands late).
    assert_eq!(
        m.run_until(Moment(4)).unwrap(),
        Exit::Common(CommonExit::Deadline {
            reached: Moment(99)
        }),
        "run_until folds reached 99 with deadline 4 → 99"
    );
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
    m.push_exit(Exit::Common(CommonExit::Idle));
    assert_eq!(m.run().expect("run"), Exit::Common(CommonExit::Idle));
    // Only the last-set vector is accepted; 0x40 was never injected.
    assert_eq!(m.take_accepted_interrupt(), Some(0x50));
    assert_eq!(m.take_accepted_interrupt(), None);

    // Re-arbitrating to `None` retracts a stale vector (the P2 fix): it is not
    // accepted, even though the backend would otherwise be injectable.
    let mut m = configured();
    m.set_pending_irq(Some(0x40)).unwrap();
    m.set_pending_irq(None).unwrap(); // TPR raised / vector no longer deliverable
    assert_eq!(m.pending_irq(), None);
    m.push_exit(Exit::Common(CommonExit::Idle));
    assert_eq!(m.run().expect("run"), Exit::Common(CommonExit::Idle));
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
    m.push_exit(Exit::Common(CommonExit::Idle));
    assert_eq!(m.run().expect("run"), Exit::Common(CommonExit::Idle));
    assert_eq!(
        m.take_accepted_interrupt(),
        None,
        "held pending while deferred"
    );
    assert_eq!(m.pending_irq(), Some(0x40), "still pending (un-accepted)");
    // Re-enable acceptance; the next entry accepts it.
    m.set_defer_accept(false);
    m.push_exit(Exit::Common(CommonExit::Idle));
    assert_eq!(m.run().expect("run"), Exit::Common(CommonExit::Idle));
    assert_eq!(m.take_accepted_interrupt(), Some(0x40));
}

#[test]
fn mock_observability_and_config_getters() {
    // with_capabilities overrides the reported caps.
    let caps = Capabilities {
        name: "test-mock",
        deterministic_rng: false,
        arch: X86Caps {
            deterministic_tsc: true,
            enforces_tsc_deadline_msr: true,
        },
    };
    assert_eq!(MockBackend::with_capabilities(caps).capabilities(), caps);

    let mut m = MockBackend::new();
    assert!(m.installed_cpuid().is_none());
    assert!(m.installed_msr_filter().is_none());
    assert!(!m.is_configured());

    let policy = X86Policy {
        cpuid: CpuidModel::default(),
        msr_filter: MsrFilter::default(),
    };
    m.set_policy(&policy).unwrap();
    assert_eq!(m.installed_cpuid(), Some(&policy.cpuid));
    assert_eq!(m.installed_msr_filter(), Some(&policy.msr_filter));
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
/// and `Box<dyn Backend<A = X86>>` **forwards** the harvest to the inner impl instead of
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
    let mut boxed: Box<dyn Backend<A = X86>> = Box::new(m);
    assert_eq!(boxed.harvest_dirty_gfns().unwrap(), vec![2, 9]);
    // Drained: the next harvest window is empty, not a replay of the last.
    assert_eq!(boxed.harvest_dirty_gfns().unwrap(), Vec::<u64>::new());
}

// ---------------------------------------------------------------------------
// Gate 2 — the core run-loop / completion proptest (≥256 cases).
// ---------------------------------------------------------------------------

/// An arbitrary `Exit<X86>` spanning every variant.
fn arb_exit() -> impl Strategy<Value = Exit<X86>> {
    prop_oneof![
        (any::<u16>(), 1u8..=4, any::<Option<u32>>())
            .prop_map(|(port, size, write)| Exit::Arch(X86Exit::Io { port, size, write })),
        (any::<u64>(), 1u8..=8, any::<Option<u64>>()).prop_map(|(g, size, write)| Exit::Common(
            CommonExit::Mmio {
                gpa: Gpa(g),
                size,
                write
            }
        )),
        any::<u32>().prop_map(|index| Exit::Arch(X86Exit::Rdmsr { index })),
        (any::<u32>(), any::<u64>())
            .prop_map(|(index, value)| Exit::Arch(X86Exit::Wrmsr { index, value })),
        any::<[u64; 4]>()
            .prop_map(|r| Exit::Common(CommonExit::Hypercall(HypercallFrame { args: r }))),
        (any::<u32>(), any::<u32>())
            .prop_map(|(leaf, subleaf)| Exit::Arch(X86Exit::Cpuid { leaf, subleaf })),
        Just(Exit::Arch(X86Exit::Rdtsc)),
        Just(Exit::Arch(X86Exit::Rdtscp)),
        (2u8..=8).prop_map(|width| Exit::Arch(X86Exit::Rdrand { width })),
        (2u8..=8).prop_map(|width| Exit::Arch(X86Exit::Rdseed { width })),
        Just(Exit::Common(CommonExit::Idle)),
        Just(Exit::Common(CommonExit::Shutdown)),
        any::<u64>().prop_map(|v| Exit::Common(CommonExit::Deadline { reached: Moment(v) })),
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
        m.push_exit(Exit::Common(CommonExit::Shutdown));

        for scripted in &script {
            let got = m.run().expect("run");
            let needs_completion = m.has_pending();
            // The pending flag is exactly "this exit needs a completion".
            let is_read_style = matches!(scripted,
                Exit::Arch(X86Exit::Io { write: None, .. }) | Exit::Common(CommonExit::Mmio { write: None, .. })
                | Exit::Arch(X86Exit::Rdmsr { .. }) | Exit::Arch(X86Exit::Wrmsr { .. }) | Exit::Common(CommonExit::Hypercall(_))
                | Exit::Arch(X86Exit::Cpuid { .. }) | Exit::Arch(X86Exit::Rdtsc) | Exit::Arch(X86Exit::Rdtscp)
                | Exit::Arch(X86Exit::Rdrand { .. }) | Exit::Arch(X86Exit::Rdseed { .. }));
            prop_assert_eq!(needs_completion, is_read_style);

            if needs_completion {
                // Resuming without completing fails closed...
                prop_assert!(matches!(m.run(), Err(BackendError::PendingCompletion)));
                // ...and a correct completion clears it.
                complete_correctly(&mut m, &got).expect("complete");
                prop_assert!(!m.has_pending());
            }
        }
        prop_assert_eq!(m.run().expect("trailing run"), Exit::Common(CommonExit::Shutdown));
    }
}
