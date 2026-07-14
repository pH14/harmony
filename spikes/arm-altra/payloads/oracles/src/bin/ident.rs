// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ident` — the guest-side capability report. No counting window.
//!
//! This is the payload to run first on arrival day: it proves the whole toolchain
//! (build, boot, MMU, console, exit status) works on the real box before a single
//! measurement is spent, and it reports, *from inside the guest*, the ID-register
//! facts stage AA-0's truth table asserts from the host:
//!
//! - `MIDR_EL1` — the part. Neoverse N1 is the rr-characterized lineage
//!   (`docs/ARM-PORT.md` §evidence).
//! - `ID_AA64ISAR0_EL1.Atomic` — **expect present**. LSE is what makes the
//!   LSE-only guest contract (AA-4) possible at all.
//! - `ID_AA64MMFR0_EL1.ECV` — **expect absent**. Its absence is the premise of the
//!   entire paravirt-clock design (`docs/ARM-ALTRA.md` §1); if it were present,
//!   that is a favorable deviation which still requires a recorded ruling before
//!   any stage leans on it.
//! - `ID_AA64DFR0_EL1.PMUVer` — the PMU version behind the `BR_RETIRED` bet.
//! - `ID_AA64PFR0_EL1.SVE` — **expect absent** on N1. Its presence on Graviton
//!   V1/V2 is the rr-flagged non-faulting-load worry a Graviton re-run must not
//!   skip (`docs/ARM-ALTRA.md` §6).
//! - `PMCEID0_EL0` bit 0x21 — is `BR_RETIRED` even implemented here.
//!
//! Read from the guest, these are the *guest-visible* values — which is exactly
//! what AA-6(a) needs when it checks that a synthetic `ID_AA64*` model was really
//! installed. On QEMU they are the emulated CPU's values and prove nothing about
//! N1.

#![no_std]
#![no_main]

use runtime::{payload, println};

const NAME: &str = "ident";

/// Read a system register by name into a `u64`.
macro_rules! mrs {
    ($reg:literal) => {{
        let value: u64;
        // SAFETY: a read of an EL1-readable ID register. No memory effects.
        unsafe {
            core::arch::asm!(
                concat!("mrs {v}, ", $reg),
                v = out(reg) value,
                options(nomem, nostack, preserves_flags),
            );
        }
        value
    }};
}

/// Extract a 4-bit ID-register field.
fn field(value: u64, shift: u32) -> u64 {
    (value >> shift) & 0xf
}

#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    payload::start(NAME);

    let midr = mrs!("midr_el1");
    let isar0 = mrs!("id_aa64isar0_el1");
    let mmfr0 = mrs!("id_aa64mmfr0_el1");
    let dfr0 = mrs!("id_aa64dfr0_el1");
    let pfr0 = mrs!("id_aa64pfr0_el1");
    let pmceid0 = mrs!("pmceid0_el0");

    println!("ID midr={midr:#x}");
    println!("ID id_aa64isar0={isar0:#x}");
    println!("ID id_aa64mmfr0={mmfr0:#x}");
    println!("ID id_aa64dfr0={dfr0:#x}");
    println!("ID id_aa64pfr0={pfr0:#x}");
    println!("ID pmceid0={pmceid0:#x}");

    // Decoded, as expect-vs-found rows. The *expectations* are N1's
    // (`docs/ARM-ALTRA.md` AA-0); under TCG they will not all hold, and that is
    // fine — this payload records, it does not judge. Only silicon judges.
    let lse = field(isar0, 20); // ID_AA64ISAR0_EL1.Atomic
    let ecv = field(mmfr0, 60); // ID_AA64MMFR0_EL1.ECV
    let pmuver = field(dfr0, 8); // ID_AA64DFR0_EL1.PMUVer
    let sve = field(pfr0, 32); // ID_AA64PFR0_EL1.SVE
    let br_retired = (pmceid0 >> 0x21) & 1; // PMCEID0_EL0 bit 0x21 == BR_RETIRED

    println!("CAP lse={lse} expect=present");
    println!("CAP ecv={ecv} expect=absent");
    println!("CAP pmuver={pmuver} expect=nonzero");
    println!("CAP sve={sve} expect=absent");
    println!("CAP br_retired_implemented={br_retired} expect=1");

    // The one hard requirement even under emulation: the runtime came up and the
    // console round-trips. Capability judgements belong to AA-0 on real silicon,
    // not to a payload running on an emulated CPU — so this payload never FAILs on
    // a capability row.
    payload::ok("ident-reported");
    payload::pass(NAME)
}
