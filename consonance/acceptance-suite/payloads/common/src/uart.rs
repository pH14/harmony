//! Polled 8250 UART console (COM1 at port 0x3F8). No UART interrupts: IER is
//! zeroed and every byte spins on THR-empty.

use crate::io::{inb, outb};

const COM1: u16 = 0x3F8;

/// Program 115200 8N1 with FIFOs on and all UART interrupts off.
pub fn init() {
    outb(COM1 + 1, 0x00); // IER: no interrupts, polled only
    outb(COM1 + 3, 0x80); // LCR: DLAB=1
    outb(COM1, 0x01); // divisor 1 = 115200 baud
    outb(COM1 + 1, 0x00);
    outb(COM1 + 3, 0x03); // LCR: 8N1, DLAB=0
    outb(COM1 + 2, 0xC7); // FCR: enable + reset FIFOs
    outb(COM1 + 4, 0x03); // MCR: DTR | RTS
}

fn putb(b: u8) {
    while inb(COM1 + 5) & 0x20 == 0 {} // LSR bit 5: THR empty
    outb(COM1, b);
}

/// `core::fmt::Write` sink for the console; used via [`crate::println!`].
pub struct Uart;

impl core::fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.bytes() {
            putb(b);
        }
        Ok(())
    }
}

/// Print one line to the UART console (LF line endings, matching the
/// committed goldens byte for byte).
#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => {{
        use ::core::fmt::Write as _;
        // Uart::write_str never errors, so the Result is statically Ok.
        let _ = ::core::writeln!($crate::uart::Uart, $($arg)*);
    }};
}
