//! `compute`: the deterministic integer workhorse. Runs the shared
//! compute-core workload (xorshift64* driving 10M add/xor/rotate steps over
//! eight registers and a 1 MiB scratch buffer) and prints the digest. The
//! same digest is independently computed by compute-core's host-side test,
//! so "same work => same state" is checked against the committed golden.
#![no_std]
#![no_main]

use compute_core::SCRATCH_LEN;

/// 1 MiB zero-initialized scratch buffer (.bss).
struct Scratch(core::cell::UnsafeCell<[u8; SCRATCH_LEN]>);

// SAFETY: single vCPU; payload_main takes the only reference, once.
unsafe impl Sync for Scratch {}

static SCRATCH: Scratch = Scratch(core::cell::UnsafeCell::new([0; SCRATCH_LEN]));

/// Entry point, called by common's boot shim once in 64-bit long mode.
#[unsafe(no_mangle)]
extern "C" fn payload_main() -> ! {
    common::payload::start("compute");
    // SAFETY: see Scratch — this is the only reference ever created.
    let scratch = unsafe { &mut *SCRATCH.0.get() };
    let digest = compute_core::run(scratch);
    common::println!("DIGEST {digest:016x}");
    common::payload::pass("compute")
}
