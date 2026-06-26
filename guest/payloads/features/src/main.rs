//! `features`: exercises the nondeterministic-instruction surface the
//! hypervisor will trap — CPUID stability, RDRAND/RDSEED, RDPMC-#GP — with
//! environment-independent output. Values are never printed; only whether
//! each architectural contract held. The same goldens must pass under QEMU
//! TCG and, later, under the deterministic hypervisor.
#![no_std]
#![no_main]

use core::arch::x86_64::__cpuid_count;
use core::sync::atomic::Ordering::SeqCst;

use common::idt;

const NAME: &str = "features";

/// Fixed CPUID (leaf, subleaf) set probed for stability.
const CPUID_LEAVES: [(u32, u32); 5] = [(0, 0), (1, 0), (7, 0), (0x8000_0000, 0), (0x8000_0001, 0)];

fn cpuid_snapshot() -> [[u32; 4]; CPUID_LEAVES.len()] {
    let mut out = [[0u32; 4]; CPUID_LEAVES.len()];
    for (slot, &(leaf, subleaf)) in out.iter_mut().zip(CPUID_LEAVES.iter()) {
        // Leaves beyond the supported maximum return defined (stable) values.
        let r = __cpuid_count(leaf, subleaf);
        *slot = [r.eax, r.ebx, r.ecx, r.edx];
    }
    out
}

/// One RDRAND attempt (32-bit form, 3 bytes: 0F C7 F0). Returns CF.
fn rdrand_once() -> bool {
    let ok: u8;
    // SAFETY: a fault is caught by the #UD/#GP stubs and skipped
    // (FAULT_SKIP is set to this instruction's length by the caller).
    unsafe {
        core::arch::asm!(
            "rdrand eax",
            "setc dl",
            out("eax") _,
            out("dl") ok,
            options(nomem, nostack),
        );
    }
    ok != 0
}

/// One RDSEED attempt (32-bit form, 3 bytes: 0F C7 F8). Returns CF.
fn rdseed_once() -> bool {
    let ok: u8;
    // SAFETY: as rdrand_once.
    unsafe {
        core::arch::asm!(
            "rdseed eax",
            "setc dl",
            out("eax") _,
            out("dl") ok,
            options(nomem, nostack),
        );
    }
    ok != 0
}

/// Probe one advertised random-source instruction: retry per spec until
/// CF=1. Faulting or never succeeding is "advertised but broken" => FAIL.
fn probe_random_source(
    check: &str,
    advertised: bool,
    once: fn() -> bool,
    instr_len: u64,
    retries: u32,
) {
    if advertised {
        idt::FAULT_SKIP.store(instr_len, SeqCst);
        let faults_before = idt::FAULT_COUNT.load(SeqCst);
        let mut succeeded = false;
        for _ in 0..retries {
            let cf = once();
            if idt::FAULT_COUNT.load(SeqCst) != faults_before {
                common::payload::fail(NAME, "advertised instruction faulted");
            }
            if cf {
                succeeded = true;
                break;
            }
            // SAFETY: pause is a spin-wait hint with no other effects.
            unsafe { core::arch::asm!("pause", options(nomem, nostack)) };
        }
        if !succeeded {
            common::payload::fail(NAME, "advertised instruction never set CF");
        }
    }
    // "OK" either way: advertised-and-works, or not advertised at all.
    common::payload::ok(check);
}

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);

    // Fault handlers for the probes below (reuses the IDT machinery the
    // `interrupts` payload exercises).
    idt::set_gate(6, idt::ud_stub);
    idt::set_gate(13, idt::gp_stub);
    idt::load();

    // CPUID executed twice over a fixed leaf set must be byte-identical.
    if cpuid_snapshot() != cpuid_snapshot() {
        common::payload::fail(NAME, "cpuid results changed between executions");
    }
    common::payload::ok("cpuid-stable");

    // RDRAND: CPUID.(1):ECX[30] — leaf 1 always exists. RDSEED:
    // CPUID.(7,0):EBX[18] — leaf 7 exists only when the max basic leaf
    // (CPUID.0:EAX) is >= 7; per the SDM, CPUID above the max basic leaf
    // returns the *highest* leaf's data, so an unguarded read could misread
    // garbage as "advertised" on a smaller (e.g. frozen) CPUID model, run
    // RDSEED, fault, and FAIL — environment-dependence this payload forbids.
    // RDSEED also gets a much larger retry budget than RDRAND — its entropy
    // source may be transiently dry.
    let max_basic_leaf = __cpuid_count(0, 0).eax;
    let rdrand_advertised = __cpuid_count(1, 0).ecx & (1 << 30) != 0;
    let rdseed_advertised = max_basic_leaf >= 7 && __cpuid_count(7, 0).ebx & (1 << 18) != 0;
    probe_random_source("rdrand", rdrand_advertised, rdrand_once, 3, 1024);
    probe_random_source("rdseed", rdseed_advertised, rdseed_once, 3, 100_000);

    // RDPMC with CR4.PCE clear (the boot shim never sets it). On hardware
    // this #GPs — at CPL0 because ECX selects a counter the guest was never
    // given. QEMU TCG leaves RDPMC unimplemented and raises #UD instead, so
    // the environment-independent assertion is "it faulted and execution
    // resumed", counted across both fault stubs; the line name stays
    // rdpmc-gp per the output contract.
    idt::FAULT_SKIP.store(2, SeqCst); // rdpmc is 0F 33
    let faults_before = idt::FAULT_COUNT.load(SeqCst);
    // SAFETY: the fault this is meant to raise is caught and skipped.
    unsafe {
        core::arch::asm!(
            "rdpmc",
            inout("ecx") 0u32 => _,
            out("eax") _,
            out("edx") _,
            options(nomem, nostack),
        );
    }
    if idt::FAULT_COUNT.load(SeqCst) != faults_before + 1 {
        common::payload::fail(NAME, "rdpmc did not fault");
    }
    common::payload::ok("rdpmc-gp");

    common::payload::pass(NAME)
}
