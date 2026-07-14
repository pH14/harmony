//! `branch-dense` — seven data-dependent branches per trip, four encodings.
//!
//! The accumulator this payload returns is the exact complement of its taken
//! count (each branch adds a distinct weight on its *not-taken* path), so a
//! matching accumulator proves every predicate evaluated as the model says. The
//! smoke script checks it against `oracle_model::branch_dense_accumulator` — the
//! strongest statement emulation can make about the oracle, and it still says
//! nothing about counters, which is silicon's business alone.

#![no_std]
#![no_main]

use oracle_model::{Payload, UART_BASE};
use runtime::{params, payload, println};

core::arch::global_asm!(include_str!("../asm/branch_dense.s"));

unsafe extern "C" {
    /// The counted body. See `asm/branch_dense.s`.
    fn oracle_branch_dense(uart: u64, trips: u64, seed: u64) -> u64;
}

const NAME: &str = "branch-dense";

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

    let trips = oracle_model::trips(Payload::BranchDense, p.scale);
    if trips == 0 {
        payload::fail(NAME, "zero-trips");
    }
    println!("WINDOW trips={trips}");

    // SAFETY: the counted body; `trips` is nonzero. The seed is passed through to
    // the asm's xorshift64* state, and the model is given the same one — that is
    // what makes the predicted count and the executed count the same function.
    let acc = unsafe { oracle_branch_dense(UART_BASE, trips, p.seed) };
    println!("ACC value={acc:#x}");

    payload::ok("window-complete");
    payload::pass(NAME)
}
