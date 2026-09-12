// SPDX-License-Identifier: AGPL-3.0-or-later

use vmm_backend::{
    Arm64, Arm64Exit, CommonExit, Exit, ExitCounts, ExitReason, Gpa, HypercallFrame, X86, X86Exit,
};

fn classify(exit: &Exit<X86>) -> ExitReason {
    match exit {
        Exit::Arch(X86Exit::Io { .. }) => ExitReason::Io,
        Exit::Common(CommonExit::Mmio { .. }) => ExitReason::Mmio,
        Exit::Arch(X86Exit::Rdmsr { .. }) => ExitReason::Rdmsr,
        Exit::Arch(X86Exit::Wrmsr { .. }) => ExitReason::Wrmsr,
        Exit::Common(CommonExit::Hypercall(_)) => ExitReason::Hypercall,
        Exit::Arch(X86Exit::Cpuid { .. }) => ExitReason::Cpuid,
        Exit::Common(CommonExit::Idle) => ExitReason::Idle,
        Exit::Common(CommonExit::Shutdown) => ExitReason::Shutdown,
    }
}

fn classify_arm64(exit: &Exit<Arm64>) -> ExitReason {
    match exit {
        Exit::Common(CommonExit::Mmio { .. }) => ExitReason::Mmio,
        Exit::Common(CommonExit::Hypercall(_)) => ExitReason::Hypercall,
        Exit::Common(CommonExit::Idle) => ExitReason::Idle,
        Exit::Common(CommonExit::Shutdown) => ExitReason::Shutdown,
        Exit::Arch(Arm64Exit::Sysreg { .. }) => ExitReason::Sysreg,
    }
}

fn one_of_each() -> [Exit<X86>; 8] {
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
        Exit::Common(CommonExit::Idle),
        Exit::Common(CommonExit::Shutdown),
    ]
}

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
    assert_eq!(entries.len(), 9);

    let mut reasons: Vec<ExitReason> = entries.iter().map(|(r, _)| *r).collect();
    reasons.sort();
    reasons.dedup();
    assert_eq!(
        reasons.len(),
        9,
        "every ExitReason must appear exactly once"
    );

    let expected: Vec<ExitReason> = one_of_each()
        .iter()
        .map(Exit::reason)
        .chain(arm64_one_of_each().iter().map(Exit::reason))
        .collect();
    let got: Vec<ExitReason> = entries.iter().map(|(r, _)| *r).collect();
    assert_eq!(got, expected);
}
