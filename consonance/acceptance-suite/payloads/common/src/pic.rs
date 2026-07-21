//! Legacy 8259 PIC and 8254 PIT helpers for the `interrupts` payload.

use crate::io::outb;

/// Remap the PICs (master vector base 0x20, slave 0x28) and mask every IRQ.
pub fn init_masked() {
    outb(0x20, 0x11); // ICW1: initialize, ICW4 follows
    outb(0xA0, 0x11);
    outb(0x21, 0x20); // ICW2: master vectors 0x20..0x27
    outb(0xA1, 0x28); // ICW2: slave vectors 0x28..0x2F
    outb(0x21, 0x04); // ICW3: slave on master IRQ2
    outb(0xA1, 0x02); // ICW3: slave cascade identity
    outb(0x21, 0x01); // ICW4: 8086 mode
    outb(0xA1, 0x01);
    mask_all();
}

/// Unmask IRQ0 (the PIT) only; everything else stays masked.
pub fn unmask_irq0() {
    outb(0x21, 0xFE);
}

/// Mask every IRQ on both PICs.
pub fn mask_all() {
    outb(0x21, 0xFF);
    outb(0xA1, 0xFF);
}

/// PIT input clock in Hz.
const PIT_HZ: u32 = 1_193_182;

/// Program PIT channel 0 as a rate generator (mode 2) at ~100 Hz
/// (divisor 11931).
pub fn pit_start_100hz() {
    let divisor = (PIT_HZ / 100) as u16;
    outb(0x43, 0x34); // channel 0, lobyte/hibyte, mode 2, binary
    outb(0x40, (divisor & 0xFF) as u8);
    outb(0x40, (divisor >> 8) as u8);
}
