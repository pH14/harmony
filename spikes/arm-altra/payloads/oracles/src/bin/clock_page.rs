// SPDX-License-Identifier: AGPL-3.0-or-later
//! `clock-page` — AA-5's payload: seqlock reads of the work-derived clock page.
//!
//! Reads a *materialized* V-time — no arithmetic against any live hardware
//! counter (`docs/PARAVIRT-CLOCK.md` §0). That is the only shape of deterministic
//! time available on silicon whose counter cannot be trapped, which is every
//! reachable ARM server part.

#![no_std]
#![no_main]

use oracle_model::{PVCLOCK_ABI, PVCLOCK_GPA, Payload, UART_BASE};
use runtime::{params, payload, println, pvclock};

core::arch::global_asm!(include_str!("../asm/clock_page.s"));

unsafe extern "C" {
    /// The counted body. See `asm/clock_page.s`.
    fn oracle_clock_page(uart: u64, trips: u64, page: u64) -> u64;
}

const NAME: &str = "clock-page";

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

    // Under the harness the page is already published; under TCG nobody published
    // one, so we do — and say so. AA-5's acceptance requires `work-derived` (a page the
    // harness refreshes from work, `hm-8h8`); the spike's static harness page reports
    // `managed-static` (plumbing OK, clock unfulfilled), and the TCG golden requires
    // `self-seeded`. None can masquerade as another.
    let mode = pvclock::ensure();
    let (abi, flags) = pvclock::header();
    println!("CLOCKPAGE mode={} abi={abi} flags={flags:#x}", mode.token());

    if abi != PVCLOCK_ABI {
        // An ABI mismatch is a guest-side hard fault, never a silent reinterpret
        // (docs/PARAVIRT-CLOCK.md §1).
        payload::fail(NAME, "pvclock-abi-mismatch");
    }
    if flags & pvclock::FLAG_MATERIALIZED == 0 {
        // Without MATERIALIZED the guest would be entitled to interpolate against
        // a live counter — the one thing this design exists to forbid.
        payload::fail(NAME, "pvclock-not-materialized");
    }

    let trips = oracle_model::trips(Payload::ClockPage, p.scale);
    if trips == 0 {
        payload::fail(NAME, "zero-trips");
    }
    println!("WINDOW trips={trips}");

    // SAFETY: the counted body. The page is published (checked above) and, inside
    // the window, quiescent: the harness can only write it at a guest exit and the
    // window contains none.
    let retries = unsafe { oracle_clock_page(UART_BASE, trips, PVCLOCK_GPA) };
    println!("CLOCKPAGE retries={retries}");

    // The quiescence argument above says this must be zero. Checking it is what
    // turns the argument into a claim that can fail: a nonzero count falsifies it
    // loudly rather than quietly perturbing the oracle by an unpredictable number
    // of taken branches.
    if retries != 0 {
        payload::fail(NAME, "seqlock-retried-inside-window");
    }
    payload::ok("seqlock-quiescent");
    payload::pass(NAME)
}
