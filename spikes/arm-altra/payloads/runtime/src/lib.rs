// SPDX-License-Identifier: AGPL-3.0-or-later
//! The minimal arm64 bare-metal runtime the oracle payloads boot on.
//!
//! Boot shim (EL1, MMU on, identity-mapped low 2 GiB), a default exception vector
//! table, a polled PL011 console, GICv3 init, the two harness-shared pages
//! (params and the work-derived clock), and the exit protocol. Everything the
//! payloads need and nothing they do not — this is spike apparatus, built so that
//! Altra arrival day is `scp + run` rather than scaffolding
//! (`docs/ARM-ALTRA.md` §Immediate focus).
//!
//! **Untested on silicon.** Booted under `qemu-system-aarch64` (TCG) only.
//!
//! # The one MMIO window
//!
//! The guest touches exactly one device: the PL011 at
//! [`oracle_model::UART_BASE`]. QEMU `virt` maps a real PL011 there; the KVM
//! harness models one at the same GPA. That single window is also the harness's
//! **counter-read point** — the payload opens and closes its counting window by
//! storing [`oracle_model::MARK_BEGIN`] / [`oracle_model::MARK_END`] to the data
//! register, and each store is an MMIO exit at which the harness samples
//! `BR_RETIRED`. One device, two environments, byte-identical payloads.
//!
//! # `unsafe`, and why this crate is Miri-exempt
//!
//! Granted for this directory by task 109 §Constraints (bare-metal MMIO, system
//! registers, raw asm). It is confined to this crate and the payload asm; the
//! harness's orchestration, scanning and checking logic contains none.
//!
//! **This crate cannot run under Miri, and the reason is intrinsic, not an
//! omission.** It is `#![no_std]`, targets `aarch64-unknown-none`, and *every* unsafe
//! operation it performs is one the interpreter fundamentally cannot execute:
//!
//! - inline `asm!` / `global_asm!` (`boot.s`, `vectors.s`, the `HLT` exit, the `MRS`
//!   system-register reads) — Miri does not execute machine code;
//! - `read_volatile`/`write_volatile` to **fixed physical addresses** — the GICv3
//!   distributor at `0x0800_0000` (`gic.rs`), the PL011 at `UART_BASE` (`uart.rs`),
//!   and the two harness-shared pages at `PARAMS_GPA`/`PVCLOCK_GPA`
//!   (`params.rs`/`pvclock.rs`). Miri has no such memory; those pointers are invalid
//!   under it.
//!
//! The repository's unsafe⇒Miri contract's own escape hatch — "privileged or
//! inline-assembly operations behind a `cfg(miri)` seam, with the pointer logic
//! driven by an in-process loopback" — presupposes there is *non-privileged* logic
//! left to interpret once the privileged parts are seamed out. The crate cannot even
//! be *built* for a Miri host target: the top-level `global_asm!` (`boot.s`,
//! `vectors.s`) is aarch64 machine code the host assembler rejects, so `cargo miri
//! test` on this crate does not compile, let alone run.
//!
//! What Miri *can* check is the one piece of non-privileged logic these payloads have:
//! the **byte layout** of the harness-shared clock page. That is now seamed into
//! [`oracle_model::pvclock`] — a pure `[u8]` packing with no MMIO — and is interpreted
//! under Miri there (the nightly Miri job runs `oracle-model`). Both writers of the
//! page, this runtime's self-seeded fallback (`pvclock.rs`) and the harness's managed
//! publish (`arm-harness`), build from that one shared, Miri-checked layout; the code
//! here only writes it through the seqlock protocol to a fixed GPA. The rest of the
//! crate's `unsafe` — `asm!`/`global_asm!` and fixed-address volatile MMIO — is machine
//! code and physical memory Miri fundamentally cannot execute or model (those pointers
//! are integer literals with no Rust provenance to validate), and it is exercised by
//! the **TCG smoke** (`payloads/smoke.sh`) on an emulated N1 instead. The wider
//! Miri-checkable harness logic (the ELF loader, the `kvm_run` decode, the state
//! digest) lives in `arm-harness` and is gated under Miri there. The same applies to
//! the `oracles` crate, whose thin payload `main`s are aarch64 asm loops that call
//! straight into this runtime.

#![no_std]

core::arch::global_asm!(include_str!("boot.s"));
core::arch::global_asm!(include_str!("vectors.s"));

pub mod gic;
pub mod params;
pub mod payload;
pub mod pvclock;
pub mod uart;

use core::panic::PanicInfo;

/// Bring the runtime up. Called by the boot shim, once, before `payload_main`.
///
/// Installs the default vector table, the console and the GIC. The MMU is already
/// on by this point (the boot shim does it in asm, before any Rust runs).
#[unsafe(no_mangle)]
extern "C" fn runtime_init() {
    unsafe extern "C" {
        /// The 2 KiB-aligned default vector table from `vectors.s`.
        static __runtime_vectors: u8;
    }

    // SAFETY: `__runtime_vectors` is the linker-placed vector table, aligned to
    // 2 KiB by `linker.ld`'s `.vectors : ALIGN(2048)` — VBAR_EL1's low 11 bits
    // are RES0, so the alignment is a hard requirement and is met by construction.
    unsafe {
        let vbar = &raw const __runtime_vectors as u64;
        core::arch::asm!(
            "msr vbar_el1, {v}",
            "isb",
            v = in(reg) vbar,
            options(nostack, preserves_flags),
        );
    }

    uart::init();
    gic::init();
}

/// Every entry of the default vector table lands here.
///
/// Nothing outside a payload's counting window should ever take an exception, so
/// reaching this is a payload or runtime bug. Print the syndrome and fail loudly:
/// a spike harness that swallows an unexpected exception would be the exact
/// "green on a failed gate" pathology `docs/ARM-ALTRA.md` §Evidence integrity
/// exists to kill.
#[unsafe(no_mangle)]
extern "C" fn runtime_unexpected_exception() -> ! {
    let esr: u64;
    let elr: u64;
    let far: u64;
    // SAFETY: three reads of EL1 syndrome registers, always readable at EL1.
    unsafe {
        core::arch::asm!(
            "mrs {esr}, esr_el1",
            "mrs {elr}, elr_el1",
            "mrs {far}, far_el1",
            esr = out(reg) esr,
            elr = out(reg) elr,
            far = out(reg) far,
            options(nostack, nomem, preserves_flags),
        );
    }
    println!("UNEXPECTED EXCEPTION esr={esr:#x} elr={elr:#x} far={far:#x}");
    payload::fail_now("unexpected-exception")
}

/// Park the CPU forever. Only reached if the exit protocol's consumer is absent
/// in both environments, which would itself be a harness bug.
pub fn park() -> ! {
    loop {
        // SAFETY: `wfi` with interrupts masked parks the core; no memory effects.
        unsafe { core::arch::asm!("wfi", options(nomem, nostack, preserves_flags)) };
    }
}

/// A panicking payload prints a FAIL line and exits nonzero. Never part of golden
/// output.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("PAYLOAD panic FAIL {}", info.message());
    payload::exit(1)
}
