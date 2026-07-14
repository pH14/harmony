//! `lse-atomics` — AA-4's (b) payload: the answer.
//!
//! The same increment as `llsc-atomics`, performed by one `LDADD`. No monitor, no
//! retry, no branch whose taken-count depends on anything but the trip count. The
//! a/b pair *is* AA-4's argument.

#![no_std]
#![no_main]

use oracle_model::{Payload, UART_BASE};
use runtime::{params, payload, println};

core::arch::global_asm!(include_str!("../asm/lse_atomics.s"));

unsafe extern "C" {
    /// The counted body. See `asm/lse_atomics.s`.
    fn oracle_lse_atomics(uart: u64, trips: u64, counter: u64) -> u64;
}

const NAME: &str = "lse-atomics";

/// The word the atomic operates on. Normal memory, as for `llsc-atomics`.
static mut COUNTER: u64 = 0;

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

    let trips = oracle_model::trips(Payload::LseAtomics, p.scale);
    if trips == 0 {
        payload::fail(NAME, "zero-trips");
    }
    println!("WINDOW trips={trips}");

    let counter = &raw mut COUNTER;

    // SAFETY: the counted body, as for `llsc-atomics`. LSE is architecturally
    // present from Armv8.1 and N1 is Armv8.2 — `ident` reports
    // ID_AA64ISAR0_EL1.Atomic rather than this payload assuming it.
    let last = unsafe { oracle_lse_atomics(UART_BASE, trips, counter as u64) };
    // SAFETY: the body has returned; the word is stable.
    let final_value = unsafe { core::ptr::read_volatile(counter) };

    println!("LSE last_read={last} final={final_value}");

    if final_value != trips {
        payload::fail(NAME, "counter-mismatch");
    }
    payload::ok("counter-exact");
    payload::pass(NAME)
}
