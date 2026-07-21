// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aa4-reexec` — AA-4 notifier-replacement proof payload.
//!
//! Executes one dedicated clean page, emits a console marker, and executes the
//! SAME page again — never modifying it. Under the execute guard the first call
//! scans+approves the page; the harness performs a memslot update at the marker,
//! whose mmu-notifier invalidation must force the second call to re-scan. Because
//! the page is unchanged, a second scan is attributable only to the notifier.

#![no_std]
#![no_main]

use runtime::{payload, println};

core::arch::global_asm!(include_str!("../asm/aa4_reexec.s"));

unsafe extern "C" {
    fn aa4_reexec_target() -> u64;
    // Entered only by the harness's second (writer) vCPU in the two-vCPU race, never called
    // from Rust — referenced below so `--gc-sections` keeps its page in the image.
    fn aa4_reexec_writer();
}

const NAME: &str = "aa4-reexec";

#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    payload::start(NAME);
    // The harness resolves the target/writer pages from the ELF symbols; these prints are
    // diagnostics derived from the same addresses (and keep the writer page linked in).
    let target = aa4_reexec_target as usize as u64;
    let writer = aa4_reexec_writer as usize as u64;
    println!("AA4 target={target:#x} writer={writer:#x}");

    // SAFETY: the target is a self-contained `mov x0, #1; ret` on its own page.
    let first = unsafe { aa4_reexec_target() };
    // The harness replaces the memslot at this marker exit.
    payload::ok("reexec-first");
    // SAFETY: same self-contained target; re-executed after the memslot update.
    let second = unsafe { aa4_reexec_target() };
    if first != 1 || second != 1 {
        payload::fail(NAME, "reexec-target-returned-wrong-value");
    }
    payload::ok("reexec-second");
    payload::pass(NAME)
}
