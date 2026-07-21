//! `pit-pic-stub`: the legacy 8259 PIC + 8254 PIT deterministic boot stub (R1:
//! "minimal deterministic userspace stub for early-boot probing"). It remaps and
//! masks the PIC, runs PIT channel 0 at ~100 Hz through the remapped master PIC,
//! takes a fixed number of timer ticks, and reads the port-0x61 refresh bit
//! (contract `pit-portb` = emulate-vtime). On the box the tick cadence and the
//! refresh toggle are pure functions of V-time; in-guest the
//! environment-independent shape is that the stub initialises, ticks
//! deterministically, and the ports never fault. Reported counts let the box
//! pin the cadence. O3 tag: pure.
#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::Ordering::SeqCst;

use common::report::report;
use common::{idt, io, pic};

const NAME: &str = "pit-pic-stub";
/// Timer ticks to take.
const TICKS: u64 = 8;

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);

    idt::set_gate(0x20, idt::timer_stub);
    idt::load();
    pic::init_masked();
    common::payload::ok("pic-init");

    pic::pit_start_100hz();
    pic::unmask_irq0();
    while idt::TIMER_TICKS.load(SeqCst) < TICKS {
        // SAFETY: sti;hlt waits for the next PIT tick; IRQ0 is unmasked and the
        // timer gate is set, so a wake is pending. cli restores IF.
        unsafe { asm!("sti", "hlt", "cli", options(nomem, nostack)) };
    }
    pic::mask_all();
    common::payload::ok("pit-ticks");

    // port 0x61 bit 4 (refresh toggle) is emulate-vtime: a deterministic
    // function of V-time on the box, read here without faulting.
    let portb = io::inb(0x61);
    report(u64::from(portb));
    report(idt::TIMER_TICKS.load(SeqCst));
    common::payload::ok("portb-read");

    common::payload::pass(NAME)
}
