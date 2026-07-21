//! Port I/O. The payload environment is a single vCPU with interrupts off by
//! default, so these are exposed as safe wrappers over `in`/`out`.

/// Write a byte to an I/O port.
#[inline]
pub fn outb(port: u16, value: u8) {
    // SAFETY: bare-metal single-vCPU environment; port writes cannot violate
    // Rust memory safety here.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack));
    }
}

/// Write a 32-bit dword to an I/O port.
#[inline]
pub fn outl(port: u16, value: u32) {
    // SAFETY: as `outb`; port writes cannot violate Rust memory safety here.
    unsafe {
        core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack));
    }
}

/// Read a byte from an I/O port.
#[inline]
pub fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: as `outb`.
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack));
    }
    value
}
