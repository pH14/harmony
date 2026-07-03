// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sdk-demo`: the SDK-instrumented demo guest (task 73). It drives every guest
//! SDK verb over the real `Client<VmcallTransport>` doorbell transport:
//!
//! - **catalog-at-init** — two `sometimes` points (one wired to fire, one never:
//!   the gate-6 never-fired shape), an `always` assertion, a buggify site, a
//!   state register;
//! - **buggify** — a "slow disk" site the host decides; when it fires it takes
//!   the buggy path;
//! - a planted, **buggify-gated `always` violation** — the balance invariant only
//!   breaks on the buggy path, so a run whose seed fires buggify enough surfaces
//!   `StopReason::Assertion` (gate B); a run that never fires it passes;
//! - **IJON state** (`state_max`) and the **setup-complete** lifecycle hook.
//!
//! Box-only to run: it needs the patched KVM and the vmm-core doorbell seam that
//! services `OUT 0x0CA1`. It builds standalone for `x86_64-unknown-none` as the
//! compile proof that the SDK composes into a real bare-metal payload.
#![no_std]
#![no_main]

use harmony_sdk::{Point, Sdk};
use vmcall_transport::VmcallTransport;

const NAME: &str = "sdk-demo";

/// The declared point set (registered in one Emit at `init`).
const CATALOG: &[Point] = &[
    Point::sometimes(1, "commit_seen"),   // fires every iteration
    Point::sometimes(2, "rollback_seen"), // never fires -> never-fired report
    Point::always(20, "balance_nonneg"),  // the planted invariant
    Point::buggify(50, "slow_disk"),      // the host-decided perturbation
    Point::state(40, "min_balance"),      // an IJON register
];

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);

    // SAFETY: the boot shim identity-maps the first GiB, so REQ_GPA (0x0000_E000)
    // and RESP_GPA (0x0000_F000) name mapped, zeroed guest-RAM pages this payload
    // never otherwise touches — the `VmcallTransport` page contract. The pages
    // live for the whole run (the transport outlives every SDK call).
    let transport = unsafe { VmcallTransport::new() };
    let mut sdk = match Sdk::init(transport, CATALOG) {
        Ok(s) => s,
        Err(_) => common::payload::fail(NAME, "sdk-init"),
    };

    // Seal the boot/setup prefix once the catalog is declared.
    let _ = sdk.setup_complete();

    // A deterministic little workload. `balance` starts safely positive and
    // decrements each step; only the buggify "slow disk" path double-charges it,
    // so ONLY a run that fires buggify enough can drive it below zero and trip
    // the `always` invariant. A run that never fires buggify passes cleanly.
    let mut balance: i64 = 100;
    let mut min_balance: i64 = balance;
    for _ in 0..8u32 {
        let slow = sdk.buggify(50).unwrap_or(false);
        if slow {
            balance -= 60; // the bug: the slow-disk path over-charges
        }
        balance -= 10;

        if balance < min_balance {
            min_balance = balance;
        }
        // IJON: report the running minimum (as a non-negative magnitude).
        let _ = sdk.state_max(40, min_balance.unsigned_abs());

        // `commit_seen` fires every iteration; `rollback_seen` never does.
        let _ = sdk.assert_sometimes(true, 1);

        // The planted invariant — only the buggy path can break it.
        let _ = sdk.assert_always(balance >= 0, 20);
    }

    common::payload::pass(NAME)
}
