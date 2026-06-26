//! `interrupts`: installs a 64-bit IDT, self-tests a software interrupt
//! (vector 0x40), then runs the legacy PIT at ~100 Hz and halts until at
//! least 5 timer ticks arrive. Output never contains the tick count or any
//! timing detail — this payload is replayed millions of times by the
//! precise-injection work, so only fixed `OK` lines are printed.
#![no_std]
#![no_main]

use core::sync::atomic::Ordering::SeqCst;

use common::idt;

const NAME: &str = "interrupts";

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);

    idt::set_gate(0x40, idt::swint_stub);
    idt::set_gate(0x20, idt::timer_stub);
    idt::load();

    // Software-interrupt self-test: `int 0x40` must run the handler exactly
    // once, synchronously, with IF still clear.
    // SAFETY: vector 0x40's gate was installed above.
    unsafe { core::arch::asm!("int 0x40", options(nomem, nostack)) };
    if idt::SWINT_COUNT.load(SeqCst) != 1 {
        common::payload::fail(NAME, "swint handler did not run once");
    }
    common::payload::ok("swint");

    // PIT at ~100 Hz through the remapped PIC; halt until >= 5 ticks.
    common::pic::init_masked();
    common::pic::pit_start_100hz();
    common::pic::unmask_irq0();
    while idt::TIMER_TICKS.load(SeqCst) < 5 {
        // SAFETY: sti;hlt waits for the next tick; the timer gate is set.
        unsafe { core::arch::asm!("sti", "hlt", "cli", options(nomem, nostack)) };
    }
    common::pic::mask_all();
    common::payload::ok("timer");

    common::payload::pass(NAME)
}
