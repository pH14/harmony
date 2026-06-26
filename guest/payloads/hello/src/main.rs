//! `hello`: proves the boot shim, UART console and exit path work.
#![no_std]
#![no_main]

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start("hello");
    common::payload::pass("hello")
}
