// SPDX-License-Identifier: AGPL-3.0-or-later
//! Regression coverage for the architecture-aware completion classification.
//!
//! These assertions intentionally call both the trait seam and the generic
//! [`Exit`] wrapper. A mutation that replaces either architecture's decision,
//! or drops the wrapper's forwarding, must change an observed result here.

use vmm_backend::{Arch, ArchExit, Arm64, CommonExit, Exit, Gpa, HypercallFrame, X86, X86Exit};

fn mmio_load() -> CommonExit {
    CommonExit::Mmio {
        gpa: Gpa(0x1000),
        size: 4,
        write: None,
    }
}

fn mmio_store() -> CommonExit {
    CommonExit::Mmio {
        gpa: Gpa(0x1000),
        size: 4,
        write: Some(0xfeed),
    }
}

#[test]
fn arm64_uses_the_conservative_default_for_common_exits() {
    let load = mmio_load();
    let store = mmio_store();

    assert!(<Arm64 as Arch>::stages_common_completion(&load));
    assert!(!<Arm64 as Arch>::stages_common_completion(&store));
    assert!(!<Arm64 as Arch>::stages_common_completion(
        &CommonExit::Idle
    ));
    assert!(!<Arm64 as Arch>::stages_common_completion(
        &CommonExit::Shutdown
    ));
    assert!(!<Arm64 as Arch>::stages_common_completion(
        &CommonExit::Hypercall(HypercallFrame::default(),)
    ));

    assert!(Exit::<Arm64>::Common(load).stages_completion());
    assert!(!Exit::<Arm64>::Common(store).stages_completion());
}

#[test]
fn x86_adds_mmio_store_staging_and_forwards_arch_exits() {
    let store = mmio_store();

    assert!(<X86 as Arch>::stages_common_completion(&mmio_load()));
    assert!(<X86 as Arch>::stages_common_completion(&store));
    assert!(!<X86 as Arch>::stages_common_completion(&CommonExit::Idle));
    assert!(!<X86 as Arch>::stages_common_completion(
        &CommonExit::Shutdown
    ));
    assert!(Exit::<X86>::Common(store).stages_completion());

    let arch_exits = [
        X86Exit::Io {
            port: 0x80,
            size: 1,
            write: None,
        },
        X86Exit::Io {
            port: 0x80,
            size: 1,
            write: Some(0x7f),
        },
        X86Exit::Rdmsr { index: 0x10 },
        X86Exit::Wrmsr {
            index: 0x10,
            value: 7,
        },
        X86Exit::Cpuid {
            leaf: 1,
            subleaf: 0,
        },
        X86Exit::Rdtsc,
        X86Exit::Rdtscp,
        X86Exit::Rdrand { width: 8 },
        X86Exit::Rdseed { width: 8 },
    ];
    for exit in arch_exits {
        assert!(exit.stages_completion());
        assert!(Exit::<X86>::Arch(exit).stages_completion());
    }
    assert!(!Exit::<X86>::Common(CommonExit::Idle).stages_completion());
}
