// SPDX-License-Identifier: AGPL-3.0-or-later
//! `exception-abort` — one translation fault (EC 0x25) per trip.
//!
//! The same entry/`ERET` pair as `svc` with no `SVC` instruction, at a different
//! exception class. Two payloads, one difference: that is what makes the `SVC`
//! weight identifiable rather than merely constrained.

#![no_std]
#![no_main]

use oracle_model::{Payload, UART_BASE};
use runtime::{params, payload, println};

core::arch::global_asm!(include_str!("../asm/exception_abort.s"));

unsafe extern "C" {
    /// The counted body. See `asm/exception_abort.s`.
    fn oracle_exception_abort(uart: u64, trips: u64) -> u64;
}

const NAME: &str = "exception-abort";

#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    payload::start(NAME);

    let p = params::load();
    println!(
        "PARAMS mode={} scale={} seed={:#x}",
        p.mode.token(),
        p.scale.name(),
        p.seed
    );

    let trips = oracle_model::trips(Payload::ExceptionAbort, p.scale);
    if trips == 0 {
        payload::fail(NAME, "zero-trips");
    }
    println!("WINDOW trips={trips}");

    // SAFETY: the counted body. The faulting address (0x8000_0000) is unmapped by
    // construction — the boot shim's L1 table maps exactly two 1 GiB blocks and
    // leaves L1[2] invalid — so every trip takes a translation fault into the
    // payload's own handler, which skips the load and returns.
    unsafe { oracle_exception_abort(UART_BASE, trips) };

    payload::ok("all-aborts-resumed");
    payload::pass(NAME)
}
