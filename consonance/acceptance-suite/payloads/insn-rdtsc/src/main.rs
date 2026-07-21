//! `insn-rdtsc`: RDTSC / RDTSCP sweep. In-guest it asserts only the
//! environment-independent shape — the time-stamp counter never runs backwards
//! across many reads — and reports every reading out-of-band so the box oracle
//! can pin the trap-dependent facts: strict monotonicity, deltas matching the
//! V-time formula (TSC = 2 × V-ns), and never a raw host TSC. Under stock QEMU
//! the reads are host-derived and the reports are dropped (no doorbell handler),
//! so only the monotonic shape and the PASS banner are checked. O3 tag: pure.
#![no_std]
#![no_main]

use core::arch::asm;

use common::report::report;

const NAME: &str = "insn-rdtsc";
/// Reads per monotonicity sweep.
const N: usize = 64;

fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: rdtsc reads the time-stamp counter; no memory effects.
    unsafe { asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    (u64::from(hi) << 32) | u64::from(lo)
}

/// RDTSCP: TSC plus IA32_TSC_AUX in ECX (contract: allow-stateful guest state,
/// never a host core id).
fn rdtscp() -> (u64, u32) {
    let lo: u32;
    let hi: u32;
    let aux: u32;
    // SAFETY: rdtscp reads the TSC and TSC_AUX; no memory effects. Only reached
    // when CPUID advertises it (checked below), so it never #UDs.
    unsafe {
        asm!("rdtscp", out("eax") lo, out("edx") hi, out("ecx") aux, options(nomem, nostack))
    };
    ((u64::from(hi) << 32) | u64::from(lo), aux)
}

/// CPUID.80000001H:EDX[27] — RDTSCP supported.
fn rdtscp_supported() -> bool {
    core::arch::x86_64::__cpuid(0x8000_0001).edx & (1 << 27) != 0
}

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);

    let mut samples = [0u64; N];
    for s in samples.iter_mut() {
        *s = rdtsc();
    }
    // Non-decreasing is the environment-independent invariant (host TSC under
    // QEMU is monotonic but reads can tie); strict monotonicity and the V-time
    // delta are box-checked from the reports below.
    for w in samples.windows(2) {
        if w[1] < w[0] {
            common::payload::fail(NAME, "tsc went backwards");
        }
    }
    common::payload::ok("rdtsc-monotonic");

    report(N as u64);
    for s in samples {
        report(s);
    }

    // RDTSCP returns the same V-time TSC plus TSC_AUX; verify it does not
    // precede the last RDTSC, and report (tsc, aux) for the box.
    if rdtscp_supported() {
        let (t, aux) = rdtscp();
        if t < samples[N - 1] {
            common::payload::fail(NAME, "rdtscp tsc precedes prior rdtsc");
        }
        report(t);
        report(u64::from(aux));
    }
    common::payload::ok("rdtscp");

    common::payload::pass(NAME)
}
