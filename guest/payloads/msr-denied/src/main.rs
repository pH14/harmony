//! `msr-denied`: default-deny. The contract denies every MSR index it does not
//! name (read/write → #GP). This payload probes a spread of off-contract
//! indices (`contract-data::MSR_DENIED_SAMPLE`, each proven absent from the
//! contract by a host test) for both RDMSR and WRMSR, recording whether each
//! raised #GP.
//!
//! In-guest the environment-independent fact is only that probing the denied
//! surface never panics or hangs: under stock QEMU/TCG an unknown MSR may return
//! 0 instead of #GP, so the #GP disposition is reported, not asserted. The box
//! oracle pins "every probe raised #GP". O3 tag: pure.
#![no_std]
#![no_main]

use core::arch::asm;

use common::probe;
use common::report::report;
use contract_data::MSR_DENIED_SAMPLE;

const NAME: &str = "msr-denied";

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);
    probe::install_fault_handlers();

    for &idx in MSR_DENIED_SAMPLE {
        // rdmsr / wrmsr are 2 bytes; a #GP resumes just past them.
        let read_gp = probe::gp_faulted(2, || {
            // SAFETY: the #GP this is meant to raise is caught and skipped.
            unsafe {
                asm!("rdmsr", in("ecx") idx, out("eax") _, out("edx") _, options(nomem, nostack))
            };
        });
        let write_gp = probe::gp_faulted(2, || {
            // SAFETY: writes zero to a denied index; the #GP is caught and skipped.
            unsafe {
                asm!("wrmsr", in("ecx") idx, in("eax") 0u32, in("edx") 0u32, options(nomem, nostack))
            };
        });
        report(u64::from(idx));
        report(u64::from(read_gp));
        report(u64::from(write_gp));
    }
    common::payload::ok("msr-denied-probed");

    common::payload::pass(NAME)
}
