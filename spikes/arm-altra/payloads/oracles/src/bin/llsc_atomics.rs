//! `llsc-atomics` — AA-4's (a) payload: the LL/SC hazard, demonstrated.
//!
//! The only payload whose taken-branch count is deliberately not known by
//! construction: `STXR` failure is the phenomenon under study, so the retries are
//! counted in-guest (branch-free) and reported. On a quiescent run they must be
//! zero; under AA-4(a)'s injection schedule they will not be, and quantifying that
//! divergence is the deliverable.

#![no_std]
#![no_main]

use oracle_model::{Payload, UART_BASE};
use runtime::{params, payload, println};

core::arch::global_asm!(include_str!("../asm/llsc_atomics.s"));

unsafe extern "C" {
    /// The counted body. See `asm/llsc_atomics.s`.
    fn oracle_llsc_atomics(uart: u64, trips: u64, counter: u64) -> u64;
}

const NAME: &str = "llsc-atomics";

/// The word the exclusives operate on. It lives in the image's `.bss`, which the
/// boot shim maps Normal (cacheable, inner-shareable) — exclusives on Device
/// memory are not architecturally defined, so this placement is a requirement, not
/// a convenience.
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

    let trips = oracle_model::trips(Payload::LlscAtomics, p.scale);
    if trips == 0 {
        payload::fail(NAME, "zero-trips");
    }
    println!("WINDOW trips={trips}");

    let counter = &raw mut COUNTER;

    // SAFETY: the counted body. `counter` points at a live, Normal-memory u64 that
    // nothing else touches (single vCPU, no interrupts inside the window).
    let retries = unsafe { oracle_llsc_atomics(UART_BASE, trips, counter as u64) };
    // SAFETY: the body has returned, so the exclusive sequence is complete and the
    // word is stable.
    let final_value = unsafe { core::ptr::read_volatile(counter) };

    println!("LLSC retries={retries} final={final_value}");

    // The increment must have landed exactly `trips` times whatever the retries
    // were — that is what "the retry loop is correct" means, and it is the
    // precondition for the retry count being the *only* divergence LL/SC
    // introduces. If this fails, the payload is broken and its retry number means
    // nothing.
    if final_value != trips {
        payload::fail(NAME, "counter-mismatch");
    }
    payload::ok("counter-exact");
    payload::pass(NAME)
}
