//! Minimal xAPIC (local APIC) MMIO helpers for the `irq-landing` payload.
//!
//! The contract pins the LAPIC to xAPIC mode (CPUID x2APIC bit = 0; the x2APIC
//! MSR range `0x800-0x8FF` is deny-gp) with the register file at the
//! architectural MMIO base `0xFEE00000` (`docs/cpu-msr-contract.toml` `[mmio]`
//! region `xapic`; R1 device model). The boot shim identity-maps the 4th GiB so
//! these accesses resolve. The timer is driven by the initial-count register
//! (`TIMER_ICR`, offset 0x380): on the deterministic VMM a write becomes a
//! V-time deadline; current-count is computed from V-time on read.

use core::ptr::{read_volatile, write_volatile};

/// Architectural xAPIC MMIO base (APIC global-enable + base in IA32_APIC_BASE =
/// 0xfee00900 per contract; the base is never relocated — APICBASE write is
/// deny-ignore-write).
const BASE: usize = 0xFEE0_0000;

/// Spurious-interrupt vector register (bit 8 = APIC software enable).
pub const SVR: usize = 0x0F0;
/// End-of-interrupt register (write 0 to dismiss the in-service interrupt).
pub const EOI: usize = 0x0B0;
/// Task-priority register.
pub const TPR: usize = 0x080;
/// LVT timer entry (vector + mask + mode: one-shot / periodic / TSC-deadline).
pub const LVT_TIMER: usize = 0x320;
/// Timer initial-count register — a write arms the timer (V-time deadline).
pub const TIMER_ICR: usize = 0x380;
/// Timer current-count register (emulate-vtime read).
pub const TIMER_CCR: usize = 0x390;
/// Timer divide-configuration register.
pub const TIMER_DCR: usize = 0x3E0;

/// Read a 32-bit xAPIC register at `reg` (one of the offset constants).
pub fn read(reg: usize) -> u32 {
    // SAFETY: bare-metal MMIO; the xAPIC page (0xFEE00000) is identity-mapped by
    // the boot shim and 32-bit aligned register reads have no other effect.
    unsafe { read_volatile((BASE + reg) as *const u32) }
}

/// Write `value` to the 32-bit xAPIC register at `reg`.
pub fn write(reg: usize, value: u32) {
    // SAFETY: as `read`; the xAPIC page is mapped and writable.
    unsafe { write_volatile((BASE + reg) as *mut u32, value) }
}

/// Signal end-of-interrupt to the LAPIC (offset 0xB0, write 0).
pub fn eoi() {
    write(EOI, 0);
}

/// Software-enable the APIC: set SVR bit 8 with spurious vector `spurious`, and
/// drop the task priority to 0 so all vectors are deliverable.
pub fn enable(spurious: u8) {
    write(TPR, 0);
    write(SVR, 0x100 | u32::from(spurious));
}
