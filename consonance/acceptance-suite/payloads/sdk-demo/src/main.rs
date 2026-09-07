// SPDX-License-Identifier: AGPL-3.0-or-later
//! `sdk-demo`: the generic SDK-instrumented demo guest. It drives the guest
//! SDK verb over the real `Client<VmcallTransport>` doorbell transport:
//!
//! - **catalog-at-init** — two `sometimes` points (one wired to fire, one never:
//!   the gate-6 never-fired shape), an `always` assertion, and a state register;
//! - **IJON state** (`state_max`) and the **setup-complete** lifecycle hook.
//!
//! Box-only to run: it needs the patched KVM and the vmm-core doorbell seam that
//! services `OUT 0x0CA1`. It builds standalone for `x86_64-unknown-none` as the
//! compile proof that the SDK composes into a real bare-metal payload.
#![no_std]
#![no_main]

use harmony_sdk::{Point, Sdk};
use hypercall_doorbell::VmcallTransport;

const NAME: &str = "sdk-demo";

/// The declared point set (registered in one Emit at `init`).
const CATALOG: &[Point] = &[
    Point::sometimes(1, "commit_seen"),   // fires every iteration
    Point::sometimes(2, "rollback_seen"), // never fires -> never-fired report
    Point::always(20, "balance_nonneg"),  // the invariant
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

    // Seal the boot/setup prefix once the catalog is declared — loud on error
    // (a swallowed seal failure would desync the host's setup boundary).
    if sdk.setup_complete().is_err() {
        common::payload::fail(NAME, "setup_complete");
    }

    // A deterministic little workload. `balance` starts safely positive and
    // decrements each step while the generic SDK reports its state and invariant.
    let mut balance: i64 = 100;
    let mut min_balance: i64 = balance;
    for _ in 0..8u32 {
        balance -= 10;

        if balance < min_balance {
            min_balance = balance;
        }
        // IJON: report the running minimum (as a non-negative magnitude). Loud on
        // Err like every SDK call — a swallowed error hides a broken doorbell.
        if sdk.state_max(40, min_balance.unsigned_abs()).is_err() {
            common::payload::fail(NAME, "state_max");
        }

        // `commit_seen` fires every iteration; `rollback_seen` never does.
        if sdk.assert_sometimes(true, 1).is_err() {
            common::payload::fail(NAME, "assert_sometimes");
        }

        // The generic invariant stays explicit and loud on transport failure.
        if sdk.assert_always(balance >= 0, 20).is_err() {
            common::payload::fail(NAME, "assert_always");
        }
    }

    common::payload::pass(NAME)
}
