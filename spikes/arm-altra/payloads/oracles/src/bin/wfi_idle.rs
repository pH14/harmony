//! `wfi-idle` — deterministic idle: mask, make an interrupt pending, `WFI`,
//! unmask.
//!
//! The wake source is a self-directed SGI rather than the timer, because a
//! timer-woken loop must re-check whether the timer really fired (`WFI` may
//! complete for any reason) and that re-check is a wall-clock-dependent taken
//! branch — inside a counting window, fatal to the oracle. `asm/wfi_idle.s`
//! carries the full argument and what it costs.

#![no_std]
#![no_main]

use oracle_model::{Payload, UART_BASE};
use runtime::{params, payload, println};

core::arch::global_asm!(include_str!("../asm/wfi_idle.s"));

unsafe extern "C" {
    /// The counted body. See `asm/wfi_idle.s`.
    fn oracle_wfi_idle(uart: u64, trips: u64) -> u64;
}

const NAME: &str = "wfi-idle";

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

    let trips = oracle_model::trips(Payload::WfiIdle, p.scale);
    if trips == 0 {
        payload::fail(NAME, "zero-trips");
    }
    println!("WINDOW trips={trips}");

    // SAFETY: the counted body. The GIC is already initialized by `runtime_init`
    // (SGI 1 enabled, group 1, priority below the mask), which is this body's only
    // precondition beyond a nonzero trip count.
    unsafe { oracle_wfi_idle(UART_BASE, trips) };

    // Reaching here means every trip's interrupt was delivered and acknowledged.
    // A lost interrupt would have parked the payload in `WFI` forever, and the
    // smoke's timeout — not a done-marker — would have failed it.
    payload::ok("all-interrupts-taken");
    payload::pass(NAME)
}
