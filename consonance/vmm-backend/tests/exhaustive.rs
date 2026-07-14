// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 5 — exhaustiveness. A `match` over `Exit` with **no wildcard** compiles
//! (the variant set is closed and the contract surface is complete), and
//! `ExitCounts::entries()` covers every `ExitReason` exactly once. Portable; no
//! mock needed.

use vmm_backend::{Exit, ExitCounts, ExitReason, Gpa, HypercallFrame, Moment};

/// Classify every `Exit` with a wildcard-free `match`. If a variant is ever added
/// without updating this arm, the crate stops compiling — that is the gate.
fn classify(exit: &Exit) -> ExitReason {
    match exit {
        Exit::Io { .. } => ExitReason::Io,
        Exit::Mmio { .. } => ExitReason::Mmio,
        Exit::Rdmsr { .. } => ExitReason::Rdmsr,
        Exit::Wrmsr { .. } => ExitReason::Wrmsr,
        Exit::Hypercall(_) => ExitReason::Hypercall,
        Exit::Cpuid { .. } => ExitReason::Cpuid,
        Exit::Rdtsc => ExitReason::Rdtsc,
        Exit::Rdtscp => ExitReason::Rdtscp,
        Exit::Rdrand { .. } => ExitReason::Rdrand,
        Exit::Rdseed { .. } => ExitReason::Rdseed,
        Exit::Idle => ExitReason::Idle,
        Exit::Shutdown => ExitReason::Shutdown,
        Exit::Deadline { .. } => ExitReason::Deadline,
    }
}

/// One value of every `Exit` variant — the closed set the contract enumerates.
fn one_of_each() -> [Exit; 13] {
    [
        Exit::Io {
            port: 0x80,
            size: 1,
            write: None,
        },
        Exit::Mmio {
            gpa: Gpa(0xFEE0_0000),
            size: 4,
            write: Some(0),
        },
        Exit::Rdmsr { index: 0x1B },
        Exit::Wrmsr {
            index: 0x6E0,
            value: 0,
        },
        Exit::Hypercall(HypercallFrame::default()),
        Exit::Cpuid {
            leaf: 1,
            subleaf: 0,
        },
        Exit::Rdtsc,
        Exit::Rdtscp,
        Exit::Rdrand { width: 8 },
        Exit::Rdseed { width: 8 },
        Exit::Idle,
        Exit::Shutdown,
        Exit::Deadline { reached: Moment(0) },
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
