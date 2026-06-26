//! Shared runtime for the bare-metal test payloads (task 04 Part A): the
//! Multiboot v1 boot shim (32-bit entry climbing into long mode with the
//! first GiB identity-mapped), a polled 8250 UART console, the
//! `isa-debug-exit` exit path, and a minimal 64-bit IDT with the interrupt /
//! fault stubs the `interrupts` and `features` payloads need. A payload
//! defines `#[unsafe(no_mangle)] extern "C" fn payload_main() -> !`; the shim
//! calls it once in 64-bit mode.
#![no_std]

core::arch::global_asm!(include_str!("boot.s"), options(att_syntax));

pub mod apic;
pub mod idt;
pub mod io;
pub mod payload;
pub mod pic;
pub mod probe;
pub mod report;
pub mod uart;

use core::panic::PanicInfo;

/// Write `code` to the QEMU `isa-debug-exit` port (0xF4) and park the CPU.
/// QEMU's process exit status becomes `(code << 1) | 1`; payload code 0 is
/// PASS (QEMU exit status 1).
pub fn exit(code: u8) -> ! {
    io::outb(0xF4, code);
    loop {
        // SAFETY: `cli; hlt` parks the CPU forever; only reached if the
        // isa-debug-exit device is absent.
        unsafe { core::arch::asm!("cli", "hlt", options(nomem, nostack)) };
    }
}

/// Every payload failure that is a bug (slice indexing, arithmetic overflow
/// checks, …) funnels through here. Never part of golden output: a panicking
/// payload prints a FAIL line and exits nonzero.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    crate::println!("PAYLOAD panic FAIL {}", info.message());
    exit(1)
}
