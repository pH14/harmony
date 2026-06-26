//! `insn-cpuid`: CPUID sweep over the **frozen contract model** (every concrete
//! leaf/subleaf in `contract-data`, generated from `docs/cpu-msr-contract.toml`).
//!
//! In-guest it asserts the environment-independent invariant — CPUID is stable
//! (two reads of every leaf are byte-identical) — and reports each leaf's live
//! registers plus a (matches,total) conformance tally out-of-band. The exact
//! EAX/EBX/ECX/EDX == frozen model is a box fact: under stock QEMU the values
//! are QEMU's, not the contract's, so the tally is reported (and box-checked),
//! never branched on. Dynamic registers (OSXSAVE mirror, level echo, XSAVE
//! size) are reported but excluded from the exact tally. O3 tag: pure.
#![no_std]
#![no_main]

use core::arch::x86_64::__cpuid_count;

use common::report::report;
use contract_data::CPUID_ENTRIES;

const NAME: &str = "insn-cpuid";

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);

    let mut matches: u64 = 0;
    let mut total: u64 = 0;
    for e in CPUID_ENTRIES {
        let a = __cpuid_count(e.leaf, e.subleaf);
        let b = __cpuid_count(e.leaf, e.subleaf);
        // CPUID must be a pure function of (leaf, subleaf, guest state): stable
        // across executions. This holds under QEMU and on the box.
        if (a.eax, a.ebx, a.ecx, a.edx) != (b.eax, b.ebx, b.ecx, b.edx) {
            common::payload::fail(NAME, "cpuid changed between executions");
        }
        // Report the live registers for the box oracle (leaf, subleaf, regs).
        report(u64::from(e.leaf));
        report(u64::from(e.subleaf));
        report(u64::from(a.eax));
        report(u64::from(a.ebx));
        report(u64::from(a.ecx));
        report(u64::from(a.edx));
        // Conformance tally over the static (non-dyn) registers only.
        let live = [a.eax, a.ebx, a.ecx, a.edx];
        for (i, &got) in live.iter().enumerate() {
            if !e.is_dyn(i) {
                total += 1;
                if got == e.reg(i) {
                    matches += 1;
                }
            }
        }
    }
    common::payload::ok("cpuid-stable");

    // Box oracle: matches == total against the frozen model. Not asserted here —
    // under QEMU the live model is the host's, so matches < total is expected.
    report(matches);
    report(total);

    common::payload::pass(NAME)
}
