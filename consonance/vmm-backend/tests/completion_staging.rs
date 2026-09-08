// SPDX-License-Identifier: AGPL-3.0-or-later
//! Regression coverage for the architecture-aware completion classification.
//!
//! These assertions intentionally call both the trait seam and the generic
//! [`Exit`] wrapper. A mutation that replaces either architecture's decision,
//! or drops the wrapper's forwarding, must change an observed result here.

use vmm_backend::{Arch, Arm64, CommonExit, Exit, Gpa, HypercallFrame};

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
