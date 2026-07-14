// SPDX-License-Identifier: AGPL-3.0-or-later
//! The payload output protocol and the exit path.
//!
//! First line `PAYLOAD <name> START`, then deterministic protocol lines, then
//! `PAYLOAD <name> PASS` (or `FAIL <reason>`) and `PAYLOAD EXIT <code>`.
//!
//! # Exit is a status, never a marker
//!
//! `docs/ARM-ALTRA.md` §Evidence integrity #1: *a done-marker is never a success
//! condition.* So a payload's verdict reaches its consumer as a real exit status
//! in both environments:
//!
//! - **KVM harness.** `PAYLOAD EXIT <code>` is the terminal console sentinel. The
//!   harness stops the vCPU there and takes the code as the payload's status; it
//!   never re-enters, so the semihosting call below is never executed under KVM.
//! - **QEMU/TCG.** Nobody is watching the console, so the payload additionally
//!   makes an AArch64 semihosting `SYS_EXIT` call with the same code, and QEMU's
//!   *process* exit status becomes that code. The smoke script propagates it.
//!
//! Every payload also checks its own invariants in-guest (the atomic counter
//! really reached `trips`, the clock page really carried ABI 1, …) and fails
//! nonzero if they do not hold. That is what makes the TCG smoke meaningful while
//! still claiming nothing about counts: correctness rides the exit status;
//! the console golden only pins structure.

use crate::park;
use crate::println;

/// AArch64 semihosting operation `SYS_EXIT`.
const SYS_EXIT: u64 = 0x18;
/// `ADP_Stopped_ApplicationExit` — a normal application exit, whose second
/// parameter-block word QEMU uses as the process exit status.
const ADP_STOPPED_APPLICATION_EXIT: u64 = 0x2_0026;

/// The semihosting `SYS_EXIT` parameter block.
#[repr(C)]
struct ExitBlock {
    /// [`ADP_STOPPED_APPLICATION_EXIT`].
    reason: u64,
    /// The exit status QEMU should adopt.
    code: u64,
}

/// Initialize the console and print the START banner. Call this first.
pub fn start(name: &str) {
    println!("PAYLOAD {name} START");
}

/// Print an `OK <check>` line.
pub fn ok(check: &str) {
    println!("OK {check}");
}

/// Print the PASS banner and exit 0.
pub fn pass(name: &str) -> ! {
    println!("PAYLOAD {name} PASS");
    exit(0)
}

/// Print a FAIL banner and exit 1.
pub fn fail(name: &str, reason: &str) -> ! {
    println!("PAYLOAD {name} FAIL {reason}");
    exit(1)
}

/// Fail without a payload name — for the runtime's own error paths, which run
/// before or outside any payload's protocol.
pub fn fail_now(reason: &str) -> ! {
    println!("PAYLOAD ? FAIL {reason}");
    exit(1)
}

/// Emit the terminal sentinel and stop, with `code` as the status in both
/// environments.
pub fn exit(code: u8) -> ! {
    // The KVM harness's terminal sentinel. It stops the vCPU at this store and
    // never re-enters, so nothing below runs there.
    println!("PAYLOAD EXIT {code}");

    let block = ExitBlock {
        reason: ADP_STOPPED_APPLICATION_EXIT,
        code: u64::from(code),
    };

    // SAFETY: the AArch64 semihosting call. `HLT #0xF000` is the architected
    // semihosting trap; QEMU (with `-semihosting-config enable=on`) implements it
    // and exits the process with `block.code`. On real hardware with no debugger
    // attached, HLT is UNDEFINED and traps to the guest's own vector table — but
    // this instruction is unreachable under the harness (the sentinel above
    // already stopped the vCPU), and if a harness bug ever did let it run, the
    // runtime's default vector table fails the payload loudly rather than
    // silently continuing. That is the intended failure direction.
    unsafe {
        core::arch::asm!(
            "hlt #0xF000",
            in("x0") SYS_EXIT,
            in("x1") &raw const block as u64,
            options(nostack),
        );
    }

    park()
}
