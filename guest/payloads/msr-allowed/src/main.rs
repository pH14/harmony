//! `msr-allowed`: the allowed MSR set from `docs/cpu-msr-contract.toml` (via
//! `contract-data`). Two dispositions are exercised:
//!
//! * **allow-stateful** — **every** allow-stateful index in the contract
//!   ([`MSR_ALLOWED_STATEFUL`], generated from the TOML): write a contract-legal
//!   value (canonical address, valid memory type, reserved bits clear — see
//!   [`roundtrip_value`]), read it back, restore. A round-trip is
//!   environment-independent (any correct CPU round-trips a writable MSR), so it
//!   is asserted in-guest; the swept set is gated to equal the contract's
//!   allow-stateful set in `contract-data`, so it can never silently cover a
//!   subset. This is the set Linux configures on the boot path (EFER, LSTAR, the
//!   MTRR block, CR_PAT, …), so proving it round-trips de-risks the kernel boot.
//! * **allow-fixed** (PLATFORM_INFO, MISC_ENABLE, ARCH_CAPABILITIES, …): the
//!   read returns a frozen contract value — a box fact (QEMU returns its own),
//!   so each is read under a fault catch and reported, never asserted in-guest.
//!
//! Expected values trace to the committed contract (generated in `contract-data`
//! from the TOML), not hand-entered constants. O3 tag: pure.
#![no_std]
#![no_main]

use core::arch::asm;
use core::sync::atomic::Ordering::SeqCst;

use common::report::report;
use common::{idt, probe};
use contract_data::{MSR_ALLOWED_FIXED, MSR_ALLOWED_STATEFUL, RoundtripKind, roundtrip_value};

const NAME: &str = "msr-allowed";

fn rdmsr(idx: u32) -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: bare-metal CPL0; a #GP is caught by the fault stub (FAULT_SKIP=2).
    unsafe { asm!("rdmsr", in("ecx") idx, out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    (u64::from(hi) << 32) | u64::from(lo)
}

fn wrmsr(idx: u32, val: u64) {
    let lo = val as u32;
    let hi = (val >> 32) as u32;
    // SAFETY: as rdmsr; callers write only valid (canonical) values to writable
    // MSRs and restore the original afterward.
    unsafe { asm!("wrmsr", in("ecx") idx, in("eax") lo, in("edx") hi, options(nomem, nostack)) };
}

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start(NAME);
    probe::install_fault_handlers();
    idt::FAULT_SKIP.store(2, SeqCst); // rdmsr/wrmsr are 2 bytes

    // allow-stateful round-trip over EVERY contract allow-stateful MSR
    // (environment-independent: a correct CPU round-trips a writable MSR given a
    // contract-legal value — `roundtrip_value` supplies one per index). A `#GP`
    // on an illegal write is caught and skipped by the fault stub, leaving the
    // MSR unchanged, so the read-back != written check below fails loudly.
    for &idx in MSR_ALLOWED_STATEFUL {
        let plan = roundtrip_value(idx);
        let orig = rdmsr(idx);
        let written = match plan.kind {
            // Verbatim write; or RMW toggle (EFER: flip SCE, keep LME/LMA).
            RoundtripKind::Exact => plan.value,
            RoundtripKind::Toggle => orig ^ plan.value,
        };
        wrmsr(idx, written);
        let got = rdmsr(idx);
        wrmsr(idx, orig); // restore before any failure path
        if written == orig {
            // The test value equals the live value, so a silently-dropped write
            // would still read back `written` — the round-trip proves nothing.
            // `roundtrip_value` picks values distinct from each MSR's reset/boot
            // value (and from QEMU's firmware-style MTRR defaults) precisely to
            // avoid this; a hit means that choice regressed.
            common::payload::fail(NAME, "round-trip value equals live value (vacuous)");
        }
        if got != written {
            common::payload::fail(NAME, "allow-stateful msr did not round-trip");
        }
        report(u64::from(idx));
        report(written);
    }
    common::payload::ok("msr-roundtrip");

    // allow-fixed reads: report (index, faulted, value) for the box oracle,
    // which pins value == the frozen contract read-param.
    for m in MSR_ALLOWED_FIXED {
        let mut val = 0u64;
        let faulted = probe::faulted(2, || val = rdmsr(m.index));
        report(u64::from(m.index));
        report(u64::from(faulted));
        report(val);
    }
    common::payload::ok("msr-fixed-reported");

    common::payload::pass(NAME)
}
