//! `straight-line` — 64 unbranched ALU instructions per trip, one back-edge.
//!
//! The lowest branch density in the payload set. With `branch-dense` (the
//! highest) it pins the counting window's constant offset from two directions;
//! AA-1(a) requires those two to agree, because a *variable* offset is a
//! mismatch, not a calibration.

#![no_std]
#![no_main]

use oracle_model::{Payload, UART_BASE};
use runtime::{params, payload, println};

core::arch::global_asm!(include_str!("../asm/straight_line.s"));

unsafe extern "C" {
    /// The counted body. See `asm/straight_line.s`.
    fn oracle_straight_line(uart: u64, trips: u64) -> u64;
}

const NAME: &str = "straight-line";

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

    let trips = oracle_model::trips(Payload::StraightLine, p.scale);
    if trips == 0 {
        // A zero trip count would make the loop's `subs` wrap and spin ~2^64
        // times. Fail loudly rather than hang: a hung payload on the box burns a
        // measurement window and looks like a hardware problem.
        payload::fail(NAME, "zero-trips");
    }
    println!("WINDOW trips={trips}");

    // SAFETY: the counted body. `UART_BASE` is the mapped PL011; `trips` is
    // nonzero (checked above), which is the body's only precondition.
    let acc = unsafe { oracle_straight_line(UART_BASE, trips) };
    println!("ACC value={acc:#x}");

    payload::ok("window-complete");
    payload::pass(NAME)
}
