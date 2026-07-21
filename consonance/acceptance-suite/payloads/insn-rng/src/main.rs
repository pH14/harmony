//! `insn-rng`: RDRAND / RDSEED sweep. In-guest it asserts only the
//! environment-independent contract shape — an *advertised* random source never
//! faults and eventually sets CF — and reports the produced values out-of-band.
//! The trap-dependent facts (values == the seeded contract PRNG stream, CF
//! semantics, stream advances) are box-only: under stock QEMU these are real
//! host entropy and the reports are dropped. O3 tag: rng-consuming,
//! control-flow-stable (equal work across seeds, differing output).
#![no_std]
#![no_main]

use core::arch::asm;
use core::arch::x86_64::__cpuid_count;
use core::sync::atomic::Ordering::SeqCst;

use common::report::report;
use common::{idt, probe};

const NAME: &str = "insn-rng";
/// Values collected (and reported) per source when advertised.
const COLLECT: usize = 8;

/// One RDRAND attempt (32-bit form, 3 bytes: 0F C7 F0). Returns (CF, value).
fn rdrand_once() -> (bool, u32) {
    let cf: u8;
    let v: u32;
    // SAFETY: a fault is caught by the #UD/#GP stubs and skipped (FAULT_SKIP is
    // set to this instruction's length by the caller).
    unsafe { asm!("rdrand eax", "setc dl", out("eax") v, out("dl") cf, options(nomem, nostack)) };
    (cf != 0, v)
}

/// One RDSEED attempt (32-bit form, 3 bytes: 0F C7 F8). Returns (CF, value).
fn rdseed_once() -> (bool, u32) {
    let cf: u8;
    let v: u32;
    // SAFETY: as rdrand_once.
    unsafe { asm!("rdseed eax", "setc dl", out("eax") v, out("dl") cf, options(nomem, nostack)) };
    (cf != 0, v)
}

/// Sweep one advertised random source: retry until CF=1, never faulting, and
/// report up to `COLLECT` values. Not advertised ⇒ nothing to exercise.
/// RDSEED gets a far larger retry budget; its entropy source may be dry.
fn sweep(check: &str, advertised: bool, once: fn() -> (bool, u32), retries: u32) {
    if advertised {
        idt::FAULT_SKIP.store(3, SeqCst);
        let faults0 = idt::FAULT_COUNT.load(SeqCst);
        let mut values = [0u32; COLLECT];
        let mut got = 0usize;
        let mut attempts = 0u32;
        while got < COLLECT && attempts < retries {
            attempts += 1;
            let (cf, v) = once();
            if idt::FAULT_COUNT.load(SeqCst) != faults0 {
                common::payload::fail(NAME, "advertised rng instruction faulted");
            }
            if cf {
                values[got] = v;
                got += 1;
            } else {
                // SAFETY: pause is a spin-wait hint with no other effects.
                unsafe { asm!("pause", options(nomem, nostack)) };
            }
        }
        if got == 0 {
            common::payload::fail(NAME, "advertised rng never set CF");
        }
        // Box oracle: values == the seeded contract PRNG stream (here `got` is
        // always COLLECT — the contract source sets CF immediately).
        report(got as u64);
        for &v in &values[..got] {
            report(u64::from(v));
        }
    }
    common::payload::ok(check);
}

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);
    probe::install_fault_handlers();

    // RDRAND: CPUID.1:ECX[30]. RDSEED: CPUID.7.0:EBX[18], guarded by the max
    // basic leaf so an unguarded read of leaf 7 on a smaller CPUID model can't
    // misread garbage as "advertised" (see the `features` payload rationale).
    let max_basic = __cpuid_count(0, 0).eax;
    let rdrand_adv = __cpuid_count(1, 0).ecx & (1 << 30) != 0;
    let rdseed_adv = max_basic >= 7 && __cpuid_count(7, 0).ebx & (1 << 18) != 0;
    sweep("rdrand", rdrand_adv, rdrand_once, 4096);
    sweep("rdseed", rdseed_adv, rdseed_once, 1_000_000);

    common::payload::pass(NAME)
}
