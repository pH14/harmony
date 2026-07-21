//! `insn-hlt`: HLT idle-skip. The contract intercepts HLT (hlt-exiting →
//! idle-skip): on the box the work counter freezes across the halt and V-time
//! warps to the next armed deadline, so the guest wakes at exactly that deadline
//! with no instructions retired in between. That is a box fact (it needs the
//! V-time work counter). In-guest, the environment-independent shape is that HLT
//! halts and is woken by the armed timer — exercised here against the legacy PIT
//! (mapped in the first GiB, fires under QEMU). The pre-halt work markers are
//! reported for the box to confirm idle-skip. O3 tag: pure.
#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::Ordering::SeqCst;

use common::report::report;
use common::{idt, pic};

const NAME: &str = "insn-hlt";
/// HLT/wake cycles to exercise.
const CYCLES: u64 = 4;

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);

    idt::set_gate(0x20, idt::timer_stub);
    idt::load();
    pic::init_masked();
    pic::pit_start_100hz();
    pic::unmask_irq0();

    for _ in 0..CYCLES {
        let before = idt::TIMER_TICKS.load(SeqCst);
        report(before); // box: work counter at the halt; must equal the wake value
        // Halt until the next PIT tick arrives. On the box this is idle-skip to
        // the armed deadline; under QEMU it is a real halt woken by the IRQ.
        while idt::TIMER_TICKS.load(SeqCst) == before {
            // SAFETY: sti;hlt waits for the next tick; the timer gate is set and
            // IRQ0 unmasked, so a wake is pending. cli restores the IF state.
            unsafe { asm!("sti", "hlt", "cli", options(nomem, nostack)) };
        }
    }
    pic::mask_all();
    common::payload::ok("hlt-wake");

    common::payload::pass(NAME)
}
