//! `insn-rdpmc`: RDPMC sweep. The contract has no vPMU (CPUID leaf 0xA version
//! 0) and traps RDPMC (rdpmc-exiting → #GP); the boot shim leaves CR4.PCE clear.
//! In-guest the environment-independent fact is "RDPMC faults and execution
//! resumes" — on the box a #GP, under QEMU/TCG (which leaves RDPMC
//! unimplemented) a #UD; both are counted across the fault stubs. The exact
//! #GP disposition is reported for the box. O3 tag: pure.
#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::Ordering::SeqCst;

use common::report::report;
use common::{idt, probe};

const NAME: &str = "insn-rdpmc";
/// Counter selectors probed (all denied regardless of ECX).
const SELECTORS: [u32; 3] = [0, 1, 0x4000_0000];

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);
    probe::install_fault_handlers();

    for &sel in &SELECTORS {
        let gp_before = idt::GP_COUNT.load(SeqCst);
        // rdpmc is 0F 33 (2 bytes); a fault resumes just past it.
        let faulted = probe::faulted(2, || {
            // SAFETY: the fault this is meant to raise is caught and skipped.
            unsafe {
                asm!("rdpmc", in("ecx") sel, out("eax") _, out("edx") _, options(nomem, nostack))
            };
        });
        if !faulted {
            common::payload::fail(NAME, "rdpmc did not fault");
        }
        let was_gp = idt::GP_COUNT.load(SeqCst) != gp_before;
        report(u64::from(sel));
        report(u64::from(was_gp));
    }
    common::payload::ok("rdpmc-trapped");

    common::payload::pass(NAME)
}
