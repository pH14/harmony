//! `clocks`: exercises the time sources without ever printing their values
//! (TSC values are timing-dependent; only the *checks* are in the output).
#![no_std]
#![no_main]

const NAME: &str = "clocks";

fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: rdtsc reads the time-stamp counter; no memory effects.
    unsafe {
        core::arch::asm!("rdtsc", out("eax") lo, out("edx") hi, options(nomem, nostack));
    }
    (u64::from(hi) << 32) | u64::from(lo)
}

fn cpuid_leaf0() {
    // SAFETY: CPUID leaf 0 is universally supported; rbx is preserved for LLVM.
    unsafe {
        core::arch::asm!(
            "push rbx",
            "cpuid",
            "pop rbx",
            inout("eax") 0u32 => _,
            out("ecx") _,
            out("edx") _,
            options(nomem, nostack),
        );
    }
}

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);

    // RDTSC twice: the TSC must be monotonic non-decreasing.
    let t1 = rdtsc();
    let t2 = rdtsc();
    if t2 < t1 {
        common::payload::fail(NAME, "tsc went backwards");
    }
    common::payload::ok("tsc-monotonic");

    // CPUID; RDTSC — the classic serialize-then-read pattern. The value is
    // unused; the check is that the sequence executes.
    cpuid_leaf0();
    let _ = rdtsc();
    common::payload::ok("cpuid-rdtsc");

    // One read of PIT channel 0's data port; must not fault.
    let _ = common::io::inb(0x40);
    common::payload::ok("pit-read");

    common::payload::pass(NAME)
}
