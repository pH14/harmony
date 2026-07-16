// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 5 — exhaustiveness. A `match` over each vendor's `Exit<A>` with **no
//! wildcard** compiles (the variant set is closed and the contract surface is
//! complete), and `ExitCounts::entries()` covers every `ExitReason` exactly
//! once — across **both** vendors' rosters (the counter roster names the whole
//! trapped surface; it grew its arm64 variant additively, appended after the
//! pre-arm64 prefix). Portable; no mock needed.

use vmm_backend::{
    Arm64, Arm64Exit, CommonExit, Exit, ExitCounts, ExitReason, Gpa, HypercallFrame, Moment, X86,
    X86Exit,
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

/// The arm64 twin: a wildcard-free `match` over `Exit<Arm64>` (the second
/// vendor's closed variant set — the two-level default-deny holds per vendor).
fn classify_arm64(exit: &Exit<Arm64>) -> ExitReason {
    match exit {
        Exit::Common(CommonExit::Mmio { .. }) => ExitReason::Mmio,
        Exit::Common(CommonExit::Hypercall(_)) => ExitReason::Hypercall,
        Exit::Common(CommonExit::Idle) => ExitReason::Idle,
        Exit::Common(CommonExit::Shutdown) => ExitReason::Shutdown,
        Exit::Common(CommonExit::Deadline { .. }) => ExitReason::Deadline,
        Exit::Arch(Arm64Exit::Sysreg { .. }) => ExitReason::Sysreg,
    }
}

/// One value of every `Exit<X86>` variant — the closed set the x86 contract
/// enumerates, in `ExitCounts` field order (the pre-arm64 roster prefix).
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

/// One value of every **arm64-only** `Exit<Arm64>` arch variant (the common
/// exits already appear in [`one_of_each`]; only the vendor's own variants add
/// roster entries).
fn arm64_one_of_each() -> [Exit<Arm64>; 1] {
    [Exit::Arch(Arm64Exit::Sysreg {
        sysreg: 0x0018_0000,
        write: None,
    })]
}

#[test]
fn classify_agrees_with_reason_for_every_variant() {
    for exit in &one_of_each() {
        assert_eq!(classify(exit), exit.reason());
    }
    for exit in &arm64_one_of_each() {
        assert_eq!(classify_arm64(exit), exit.reason());
    }
}

#[test]
fn exit_counts_entries_cover_every_reason_once() {
    let entries = ExitCounts::default().entries();
    assert_eq!(entries.len(), 14);

    let mut reasons: Vec<ExitReason> = entries.iter().map(|(r, _)| *r).collect();
    reasons.sort();
    reasons.dedup();
    assert_eq!(
        reasons.len(),
        14,
        "every ExitReason must appear exactly once"
    );

    // The entries' reason order matches the field order: the pre-arm64 prefix
    // (`one_of_each`) byte-for-byte, then the appended arm64 vendor reasons —
    // existing report lines never reorder.
    let expected: Vec<ExitReason> = one_of_each()
        .iter()
        .map(Exit::reason)
        .chain(arm64_one_of_each().iter().map(Exit::reason))
        .collect();
    let got: Vec<ExitReason> = entries.iter().map(|(r, _)| *r).collect();
    assert_eq!(got, expected);
}
