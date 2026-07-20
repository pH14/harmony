//! `irq-landing`: the hard core. A LAPIC one-shot timer is armed in V-time and
//! the guest spins until the interrupt lands; the box oracle measures
//! "instructions retired before the first IRQ" and pins it across runs (O1) and
//! against a golden (O2), sweeping deadlines on and around `skid_margin = 128`
//! (task 07) — the case most likely to expose a determinism bug in the
//! injection path.
//!
//! The retired-count is a box fact (it needs the V-time work counter). In-guest,
//! the environment-independent shape is that arming the xAPIC timer at each
//! deadline produces exactly one delivered interrupt — exercised here against
//! the real xAPIC MMIO (mapped by the boot shim; QEMU emulates the LAPIC timer).
//! Each armed deadline is reported so the box correlates it with the measured
//! retired-count. O3 tag: pure.
#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::Ordering::SeqCst;

use common::report::report;
use common::{apic, idt, pic};

const NAME: &str = "irq-landing";
/// LAPIC timer vector (reuses the software-interrupt vector; the PIC is masked).
const VEC: u8 = 0x40;
/// Spurious-interrupt vector.
const SPURIOUS: u8 = 0xFF;
/// One-shot deadlines (initial-count values), bracketing skid_margin = 128 ±1.
const DEADLINES: [u32; 8] = [64, 127, 128, 129, 256, 1024, 4096, 16384];
/// Spin bound: a failsafe so a never-firing timer fails cleanly instead of
/// hanging. The IRQ normally lands within a few counts, far below this.
const FAILSAFE: u64 = 200_000_000;

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);

    // Mask the legacy PIC so only the LAPIC timer delivers; install the timer
    // and spurious handlers.
    pic::init_masked();
    idt::set_gate(VEC, idt::apic_timer_stub);
    idt::set_gate(SPURIOUS, idt::swint_stub); // benign catch, no EOI for spurious
    idt::load();

    apic::enable(SPURIOUS);
    apic::write(apic::TIMER_DCR, 0b1011); // divide by 1
    apic::write(apic::LVT_TIMER, u32::from(VEC)); // one-shot, unmasked, vector

    // SAFETY: enabling interrupts; the timer and spurious gates are installed.
    unsafe { asm!("sti", options(nomem, nostack)) };
    for &deadline in &DEADLINES {
        let before = idt::APIC_TICKS.load(SeqCst);
        apic::write(apic::TIMER_ICR, deadline); // arm the one-shot countdown
        let mut spins = 0u64;
        while idt::APIC_TICKS.load(SeqCst) == before {
            // SAFETY: pause is a spin-wait hint; the IRQ interrupts this spin.
            unsafe { asm!("pause", options(nomem, nostack)) };
            spins += 1;
            if spins > FAILSAFE {
                // SAFETY: disable interrupts before bailing out.
                unsafe { asm!("cli", options(nomem, nostack)) };
                common::payload::fail(NAME, "lapic timer never fired");
            }
        }
        report(u64::from(deadline));
    }
    // SAFETY: restore interrupts-off for the exit path.
    unsafe { asm!("cli", options(nomem, nostack)) };
    common::payload::ok("irq-landing");

    common::payload::pass(NAME)
}
