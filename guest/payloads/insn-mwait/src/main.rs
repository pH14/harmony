//! `insn-mwait`: MONITOR / MWAIT / PAUSE sweep. The contract hides MONITOR
//! (CPUID.1:ECX[3]=0) and traps MONITOR/MWAIT (monitor/mwait-exiting → #UD);
//! PAUSE is a permitted spin hint. In-guest, PAUSE must be a harmless no-op and
//! MONITOR/MWAIT must not leak host time: the environment-independent fact is
//! "executed or faulted-and-resumed without hanging". The exact #UD disposition
//! is reported for the box.
//!
//! Hang-safety: MWAIT can block on a real CPU. When MONITOR is advertised
//! (only possible under a permissive QEMU CPU — never the frozen model) we arm
//! MONITOR on a scratch line and store to it first, so a *supported* MWAIT sees
//! a triggered monitor and returns immediately; when MONITOR is hidden (box and
//! default QEMU) both instructions #UD and are skipped. Either way MWAIT never
//! sleeps. O3 tag: pure.
#![no_std]
#![no_main]

use core::arch::asm;
use core::arch::x86_64::__cpuid;
use core::sync::atomic::{AtomicU32, Ordering::SeqCst};

use common::probe;
use common::report::report;

const NAME: &str = "insn-mwait";

/// The cache line MONITOR watches (only touched when MONITOR is advertised).
static MON_LINE: AtomicU32 = AtomicU32::new(0);

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);
    probe::install_fault_handlers();

    // PAUSE: a permitted no-op spin hint; must never fault.
    // SAFETY: pause has no architectural effect beyond a hint.
    unsafe { asm!("pause", options(nomem, nostack)) };
    common::payload::ok("pause");

    let addr = core::ptr::addr_of!(MON_LINE) as u64;
    let advertised = __cpuid(1).ecx & (1 << 3) != 0;

    if advertised {
        // Arm + trigger so a supported MWAIT returns immediately.
        let mon_f = probe::faulted(3, || {
            // SAFETY: a fault is caught and skipped; MONITOR only watches a
            // mapped scratch line.
            unsafe {
                asm!("monitor", in("rax") addr, in("ecx") 0u32, in("edx") 0u32, options(nostack))
            };
        });
        MON_LINE.store(1, SeqCst); // trigger the monitor before MWAIT
        let mwait_f = probe::faulted(3, || {
            // SAFETY: the monitor is already triggered, so MWAIT cannot sleep; a
            // fault is caught and skipped.
            unsafe { asm!("mwait", in("eax") 0u32, in("ecx") 0u32, options(nomem, nostack)) };
        });
        report(u64::from(mon_f));
        report(u64::from(mwait_f));
    } else {
        // Hidden (box / default QEMU): both #UD; confirm they fault and resume.
        let mon_f = probe::faulted(3, || {
            // SAFETY: the #UD this raises is caught and skipped.
            unsafe {
                asm!("monitor", in("rax") addr, in("ecx") 0u32, in("edx") 0u32, options(nostack))
            };
        });
        let mwait_f = probe::faulted(3, || {
            // SAFETY: the #UD this raises is caught and skipped.
            unsafe { asm!("mwait", in("eax") 0u32, in("ecx") 0u32, options(nomem, nostack)) };
        });
        if !mon_f || !mwait_f {
            common::payload::fail(NAME, "monitor/mwait neither executed nor faulted");
        }
        report(u64::from(mon_f));
        report(u64::from(mwait_f));
    }
    common::payload::ok("monitor-mwait");

    common::payload::pass(NAME)
}
