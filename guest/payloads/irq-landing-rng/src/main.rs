//! `irq-landing-rng`: the **seed-DEPENDENT** preemption gate — the seed-consuming
//! counterpart to the pure [`irq-landing`]. A LAPIC one-shot timer is armed in
//! V-time and the guest `pause`-spins until the interrupt lands, exactly as in
//! `irq-landing` — but each round's deadline (the timer initial-count) is derived
//! from a **seeded RDRAND draw**. Because the deadline is a pure function of the
//! seeded contract PRNG stream, the **preemption instant** (the work retired
//! before the IRQ lands) is itself a pure function of the RNG **seed**: two
//! different seeds preempt at **different** instants (the reported deadlines
//! differ), while a fixed seed repeats bit-for-bit.
//!
//! This is what makes the seed *matter to preemption*, which the pure `irq-landing`
//! cannot exercise (its deadlines are fixed, so its preemption instants are
//! seed-INVARIANT by construction). The box oracle (task 47 gate 2) asserts BOTH
//! halves on this payload: deterministic-twice at one seed, AND differing
//! preemption branch counts across seeds.
//!
//! Like `irq-landing`, the retired-count per deadline is a box fact (it needs the
//! V-time work counter); in-guest the environment-independent shape is that arming
//! the xAPIC timer at each deadline produces exactly one delivered interrupt.
//! Needs the patched determinism host (advertised RDRAND → the seeded contract
//! stream); under stock QEMU the draws are host entropy and the reports are
//! dropped. O3 tag: rng-consuming.
#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::Ordering::SeqCst;

use common::report::report;
use common::{apic, idt, pic};

const NAME: &str = "irq-landing-rng";
/// LAPIC timer vector (reuses the software-interrupt vector; the PIC is masked).
const VEC: u8 = 0x40;
/// Spurious-interrupt vector.
const SPURIOUS: u8 = 0xFF;
/// Seed-derived deadlines to arm. Four 14-bit draws make a coincidental
/// full-sequence match between two distinct seeds astronomically unlikely, so the
/// "serial differs across seeds" gate is deterministic, not flaky.
const ROUNDS: usize = 4;
/// Spin bound: a failsafe so a never-firing timer fails cleanly instead of
/// hanging. The IRQ normally lands within a few counts, far below this.
const FAILSAFE: u64 = 200_000_000;

/// One RDRAND draw (32-bit form, 3 bytes: 0F C7 F0), retrying until CF=1. On the
/// patched determinism host the value is the **seeded contract PRNG stream** (CF
/// is set immediately), so the draw — and every deadline derived from it — is
/// keyed by the RNG seed.
fn rdrand() -> u32 {
    loop {
        let cf: u8;
        let v: u32;
        // SAFETY: rdrand has no memory/stack effects; it is advertised on the
        // patched determinism host, so it never faults here.
        unsafe {
            asm!("rdrand eax", "setc dl", out("eax") v, out("dl") cf, options(nomem, nostack))
        };
        if cf != 0 {
            return v;
        }
        // SAFETY: pause is a spin-wait hint with no other effects.
        unsafe { asm!("pause", options(nomem, nostack)) };
    }
}

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
    for _ in 0..ROUNDS {
        // Seed-derived deadline in [64, 64 + 16383]: large enough to require
        // preemption (a non-exiting `pause`-spin reaches no boundary on its own),
        // small enough to land well under FAILSAFE. The draw is the seeded PRNG
        // stream, so this deadline — and thus the preemption instant — is a pure
        // function of the SEED and DIFFERS across seeds.
        let deadline: u32 = 64 + (rdrand() & 0x3FFF);
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
    common::payload::ok(NAME);

    common::payload::pass(NAME)
}
