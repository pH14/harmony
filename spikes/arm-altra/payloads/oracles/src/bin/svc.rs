// SPDX-License-Identifier: AGPL-3.0-or-later
//! `svc` — one `SVC #0` per trip into a one-instruction handler.
//!
//! Carries an exception entry, an `ERET` and an `SVC` per trip, each of unknown
//! `BR_RETIRED` weight. Differenced against `exception-abort` at equal trips it
//! isolates the `SVC` term (see `oracle-model`'s identifiability argument).

#![no_std]
#![no_main]

use oracle_model::{Payload, UART_BASE};
use runtime::{params, payload, println};

core::arch::global_asm!(include_str!("../asm/svc.s"));

unsafe extern "C" {
    /// The counted body. See `asm/svc.s`.
    fn oracle_svc(uart: u64, trips: u64) -> u64;
}

const NAME: &str = "svc";

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

    let trips = oracle_model::trips(Payload::Svc, p.scale);
    if trips == 0 {
        payload::fail(NAME, "zero-trips");
    }
    println!("WINDOW trips={trips}");

    // SAFETY: the counted body. It installs its own vector table for the duration
    // of the window and restores the runtime's before returning, so an exception
    // taken after this call still reaches the runtime's loud handler.
    unsafe { oracle_svc(UART_BASE, trips) };

    // Reaching here at all is the check: every one of `trips` SVCs was taken,
    // handled and returned from. A handler that failed to advance would have
    // looped forever; one that vectored wrong would have hit the runtime's
    // unexpected-exception handler and exited nonzero.
    payload::ok("all-svcs-returned");
    payload::pass(NAME)
}
