//! The payload output protocol: first line `PAYLOAD <name> START`, free-form
//! deterministic lines, last line `PAYLOAD <name> PASS` or
//! `PAYLOAD <name> FAIL <reason>`. No line may carry timing-, address- or
//! environment-dependent values.

use crate::{exit, println, uart};

/// Initialize the console and print the START banner. Call this first.
pub fn start(name: &str) {
    uart::init();
    println!("PAYLOAD {name} START");
}

/// Print an `OK <check>` line.
pub fn ok(check: &str) {
    println!("OK {check}");
}

/// Print the PASS banner and exit with payload code 0.
pub fn pass(name: &str) -> ! {
    println!("PAYLOAD {name} PASS");
    exit(0)
}

/// Print a FAIL banner and exit with payload code 1.
pub fn fail(name: &str, reason: &str) -> ! {
    println!("PAYLOAD {name} FAIL {reason}");
    exit(1)
}
