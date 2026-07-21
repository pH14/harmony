//! Machine-checkable value reporting to the host oracle over the dedicated
//! **report channel** (corpus box-integration).
//!
//! The instruction-sweep payloads (task 18) compute timing-independent values —
//! TSC readings, CPUID registers, MSR values, armed timer deadlines — that the
//! box oracle (`acceptance-suite` O2/conformance) pins to a golden. The Part-A serial
//! protocol forbids these in the banner (no raw TSC/IRQ counts), so they cannot
//! ride the serial lane; they ride their **own** I/O port instead.
//!
//! ## The channel — [`REPORT_PORT`] = `0x0CA2`
//!
//! Each [`report`] value is emitted as 32-bit `OUT REPORT_PORT, EAX` writes;
//! `report(u64)` is two writes — low dword then high. On the box the host
//! (`vmm-core`) surfaces each as `Exit::Io { port: 0x0CA2, size: 4, write:
//! Some(v) }` and appends `v` to an ordered report stream (an OUT needs no
//! completion). The port is **distinct from** #44's hypercall doorbell at
//! `0x0CA1`: a reported value can never be mistaken for a doorbell ring.
//!
//! Every reported value is already deterministic (a V-time TSC, a seeded-PRNG
//! word, a frozen CPUID/MSR value, a retired-instruction count) and the stream
//! is ordered by execution, so the stream is a pure function of the run — the
//! O2 conformance digest the box pins.
//!
//! ## Under stock QEMU shape-testing it is a silent no-op
//!
//! QEMU TCG has no device at `0x0CA2`, so it **discards** the `OUT` writes (no
//! `#GP`, nothing on the serial console) — exactly the no-op the Part-A serial
//! gate needs. The serial banner stays byte-identical and the two-run output
//! stays identical; only the box, where `vmm-core` captures the port, sees the
//! reported values.

use crate::io::outl;

/// The conformance report-channel I/O port (corpus box-integration). Mirrors
/// `vmm_core::devices::REPORT_PORT`; documented in `docs/INTEGRATION.md` and
/// `docs/cpu-msr-contract.toml` `[ports]`. Adjacent to but distinct from the
/// `0x0CA1` hypercall doorbell.
pub const REPORT_PORT: u16 = 0x0CA2;

/// Record one machine-checkable `value` for the box oracle: two 32-bit `OUT`
/// writes to [`REPORT_PORT`] — the low dword then the high dword. On the box the
/// host appends both to its ordered report stream; under stock QEMU (no device
/// at the port) the writes are discarded, so the Part-A serial gate is
/// unaffected. Call order *is* the on-the-wire order the box pins.
#[inline]
pub fn report(value: u64) {
    // Low dword first, then high — the host reassembles `report(u64)` from this
    // fixed (low, high) pair order.
    outl(REPORT_PORT, value as u32);
    outl(REPORT_PORT, (value >> 32) as u32);
}

/// Record a `(tag, value)` pair as **three** dwords `[tag, value_lo, value_hi]`: a
/// 32-bit discriminator (which leaf / MSR / check produced the datum) on its own
/// single dword, then the 64-bit `value` as its usual two dwords, so the box
/// oracle can attribute each datum without positional bookkeeping. The tag is one
/// `OUT` (a bare `u32`, **not** a `report(u64)` — that would emit a stray zero
/// high-dword and make the pair four dwords).
#[inline]
pub fn report_tagged(tag: u32, value: u64) {
    outl(REPORT_PORT, tag);
    report(value);
}
