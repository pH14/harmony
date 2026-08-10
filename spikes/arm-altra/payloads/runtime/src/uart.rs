// SPDX-License-Identifier: AGPL-3.0-or-later
//! The polled PL011 console — the guest's only MMIO window.
//!
//! The FIFO is deliberately **disabled** (`LCR_H.FEN = 0`). Two reasons, both
//! load-bearing:
//!
//! 1. **One byte, one MMIO exit.** Under the KVM harness each data-register store
//!    is a guest exit the harness decodes; a FIFO would let bytes batch and blur
//!    the exit at which the harness samples the work counter.
//! 2. **The window's edges must be branch-exact.** A payload's counting window is
//!    opened by a store to the data register. If the transmitter were still
//!    draining, the mark store would have to poll first — and that poll's
//!    back-edge is a *taken branch* that would land inside the window and make the
//!    count wall-clock-dependent. So the oracle asm drains the transmitter
//!    (`FR.BUSY == 0`) *before* opening the window, and closes it with a bare
//!    store that needs no poll: nothing was written in between, so the
//!    transmitter is still idle. See `payloads/README.md` §The counting window.

use core::fmt::{self, Write};
use oracle_model::UART_BASE;

/// Data register.
const DR: u64 = 0x00;
/// Flag register.
const FR: u64 = 0x18;
/// Integer baud-rate divisor.
const IBRD: u64 = 0x24;
/// Fractional baud-rate divisor.
const FBRD: u64 = 0x28;
/// Line control.
const LCR_H: u64 = 0x2c;
/// Control.
const CR: u64 = 0x30;

/// `FR.TXFF` — transmit FIFO/holding register full.
const FR_TXFF: u32 = 1 << 5;

/// `FR.BUSY` — the transmitter is still shifting. The oracle asm waits on this
/// bit (not on `TXFE`) before opening a counting window, because it is the bit
/// that means "fully drained" whether or not FIFOs are on.
pub const FR_BUSY: u32 = 1 << 3;

/// Offset of the flag register, for the oracle asm (which cannot see these
/// constants any other way).
pub const FR_OFFSET: u64 = FR;

/// # Safety
/// `off` must be a PL011 register offset. The window is mapped Device-nGnRnE by
/// the boot shim, so the access is naturally ordered and never cached.
unsafe fn write_reg(off: u64, value: u32) {
    // SAFETY: caller guarantees `off` is a valid PL011 register; UART_BASE is
    // mapped by L1[0] of the boot page table.
    unsafe { core::ptr::write_volatile((UART_BASE + off) as *mut u32, value) }
}

/// # Safety
/// As [`write_reg`].
unsafe fn read_reg(off: u64) -> u32 {
    // SAFETY: as above.
    unsafe { core::ptr::read_volatile((UART_BASE + off) as *const u32) }
}

/// Configure the PL011: 8N1, no FIFO, transmitter and receiver enabled.
pub fn init() {
    // SAFETY: all five are PL011 registers in the mapped MMIO window. Baud
    // divisors are set to a legal value but are irrelevant in both environments
    // (QEMU's model and the harness's model ignore them).
    unsafe {
        write_reg(CR, 0); // disable while reconfiguring
        write_reg(IBRD, 1);
        write_reg(FBRD, 0);
        write_reg(LCR_H, 0x60); // WLEN=8, FEN=0 (no FIFO)
        write_reg(CR, 0x301); // UARTEN | TXE | RXE
    }
}

/// Write one byte, blocking until the holding register can take it.
pub fn putb(byte: u8) {
    // SAFETY: PL011 registers, as above.
    unsafe {
        while read_reg(FR) & FR_TXFF != 0 {}
        write_reg(DR, u32::from(byte));
    }
}

/// Block until the transmitter has fully drained.
pub fn drain() {
    // SAFETY: PL011 registers, as above.
    unsafe { while read_reg(FR) & FR_BUSY != 0 {} }
}

/// A `core::fmt` sink over [`putb`].
pub struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.as_bytes() {
            putb(*byte);
        }
        Ok(())
    }
}

/// Print a formatted line to the console. The payload output protocol
/// (`payload.rs`) is built from these.
#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => {{
        use core::fmt::Write as _;
        // Writing to the console is infallible: `Console::write_str` only ever
        // returns Ok. The `let _ =` documents that, rather than unwrapping in a
        // no_std panic path.
        let _ = writeln!($crate::uart::Console, $($arg)*);
    }};
}
