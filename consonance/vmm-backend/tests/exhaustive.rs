// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 5 — exhaustiveness. A `match` over `Exit<X86>` with **no wildcard** compiles
//! (the variant set is closed and the contract surface is complete), and
//! `ExitCounts::entries()` covers every `ExitReason` exactly once. Portable; no
//! mock needed.

use vmm_backend::{
    CommonExit, Exit, ExitCounts, ExitReason, Gpa, HypercallFrame, Moment, X86, X86Exit,
};

/// Classify every `Exit<X86>` with a wildcard-free `match`. If a variant is ever added
/// without updating this arm, the crate stops compiling — that is the gate.
fn classify(exit: &Exit<X86>) -> ExitReason {
    match exit {
        Exit::Arch(X86Exit::Io { .. }) => ExitReason::Io,
        Exit::Common(CommonExit::Mmio { .. }) => ExitReason::Mmio,
        Exit::Arch(X86Exit::Rdmsr { .. }) => ExitReason::Rdmsr,
        Exit::Arch(X86Exit::Wrmsr { .. }) => ExitReason::Wrmsr,
        Exit::Common(CommonExit::Hypercall(_)) => ExitReason::Hypercall,
        Exit::Arch(X86Exit::Cpuid { .. }) => ExitReason::Cpuid,
        Exit::Arch(X86Exit::Rdtsc) => ExitReason::Rdtsc,
        Exit::Arch(X86Exit::Rdtscp) => ExitReason::Rdtscp,
        Exit::Arch(X86Exit::Rdrand { .. }) => ExitReason::Rdrand,
        Exit::Arch(X86Exit::Rdseed { .. }) => ExitReason::Rdseed,
        Exit::Common(CommonExit::Idle) => ExitReason::Idle,
        Exit::Common(CommonExit::Shutdown) => ExitReason::Shutdown,
        Exit::Common(CommonExit::Deadline { .. }) => ExitReason::Deadline,
    }
}

/// One value of every `Exit<X86>` variant — the closed set the contract enumerates.
fn one_of_each() -> [Exit<X86>; 13] {
    [
        Exit::Arch(X86Exit::Io {
            port: 0x80,
            size: 1,
            write: None,
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0xFEE0_0000),
            size: 4,
            write: Some(0),
        }),
        Exit::Arch(X86Exit::Rdmsr { index: 0x1B }),
        Exit::Arch(X86Exit::Wrmsr {
            index: 0x6E0,
            value: 0,
        }),
        Exit::Common(CommonExit::Hypercall(HypercallFrame::default())),
        Exit::Arch(X86Exit::Cpuid {
            leaf: 1,
            subleaf: 0,
        }),
        Exit::Arch(X86Exit::Rdtsc),
        Exit::Arch(X86Exit::Rdtscp),
        Exit::Arch(X86Exit::Rdrand { width: 8 }),
        Exit::Arch(X86Exit::Rdseed { width: 8 }),
        Exit::Common(CommonExit::Idle),
        Exit::Common(CommonExit::Shutdown),
        Exit::Common(CommonExit::Deadline { reached: Moment(0) }),
    ]
}

#[test]
fn classify_agrees_with_reason_for_every_variant() {
    for exit in &one_of_each() {
        assert_eq!(classify(exit), exit.reason());
    }
}

#[test]
fn exit_counts_entries_cover_every_reason_once() {
    let entries = ExitCounts::default().entries();
    assert_eq!(entries.len(), 13);

    let mut reasons: Vec<ExitReason> = entries.iter().map(|(r, _)| *r).collect();
    reasons.sort();
    reasons.dedup();
    assert_eq!(
        reasons.len(),
        13,
        "every ExitReason must appear exactly once"
    );

    // The entries' reason order matches `one_of_each` / the field order.
    let expected: Vec<ExitReason> = one_of_each().iter().map(Exit::reason).collect();
    let got: Vec<ExitReason> = entries.iter().map(|(r, _)| *r).collect();
    assert_eq!(got, expected);
}
