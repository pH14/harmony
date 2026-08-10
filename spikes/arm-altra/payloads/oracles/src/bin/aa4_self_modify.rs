// SPDX-License-Identifier: AGPL-3.0-or-later
//! `aa4-self-modify` — planted write/revoke/rescan proof payload.
//!
//! This is not an oracle-model measurement class. It executes a dedicated clean
//! page once, rewrites its first instruction from `mov x0, #1` to `mov x0, #2`,
//! performs the architected D/I-cache maintenance, and executes it again. The AA-4
//! VMM audits that page at the synchronous write and execute-guard exits.

#![no_std]
#![no_main]

use runtime::{payload, println};

core::arch::global_asm!(include_str!("../asm/aa4_self_modify.s"));

unsafe extern "C" {
    fn aa4_self_modify() -> u64;
    static aa4_self_modify_target: u8;
}

const NAME: &str = "aa4-self-modify";

#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    payload::start(NAME);
    let target = &raw const aa4_self_modify_target as u64;
    println!("AA4 target={target:#x}");

    // SAFETY: the assembly routine owns one dedicated executable page, checks its
    // initial return value, replaces one aligned instruction, performs the required
    // cache maintenance, checks the modified return value, and restores the ABI frame.
    let status = unsafe { aa4_self_modify() };
    if status != 0 {
        payload::fail(NAME, "self-modified-target-returned-wrong-value");
    }

    payload::ok("write-rescan-complete");
    payload::pass(NAME)
}
