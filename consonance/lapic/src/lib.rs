// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]
//! # lapic — userspace xAPIC register file + V-time timer
//!
//! In the deterministic hypervisor, ruling **R1** (`docs/R1-DEVICE-MODEL.md`)
//! settled that `vmm-core` runs each VM with **no in-kernel interrupt
//! controller** (`KVM_IRQCHIP_NONE`) and emulates the Local APIC **in userspace
//! as an xAPIC** — an MMIO page at `0xFEE0_0000` — with its timer driven by
//! *virtual time* rather than the host wall-clock. This crate is the
//! self-contained, pure-logic half of that emulation, in the mold of `vtime` and
//! `snapshot-store`: a deterministic state machine that takes **V-time in** (a
//! `u64` nanosecond count the caller supplies — it never reads a clock), **MMIO
//! register reads/writes in**, and produces **timer deadlines plus deliverable
//! interrupt vectors out**.
//!
//! It is the source of truth for the xAPIC register file ([`Lapic::mmio_read`] /
//! [`Lapic::mmio_write`]), prioritized interrupt delivery
//! ([`Lapic::has_deliverable`] / [`Lapic::take_interrupt`] / [`Lapic::eoi`]),
//! and the initial-count→deadline timer model ([`Lapic::advance_to`] /
//! [`Lapic::next_timer_deadline`]). Its snapshot struct ([`LapicState`]) is
//! consumed verbatim by task 09 (`vm-state`).
//!
//! ## What stays in `vmm-core`
//!
//! The KVM-facing glue is **not** here: routing `KVM_EXIT_MMIO` on the APIC page
//! into this crate, calling `KVM_INTERRUPT` with the vector
//! [`Lapic::take_interrupt`] returns, and the interrupt-window handshake all
//! stay frontier in `vmm-core`. So does ownership of `IA32_APIC_BASE` (the MMIO
//! relocation): this crate is addressed purely by offset within the page.
//!
//! ## The register model (xAPIC only)
//!
//! Per R1 and `docs/CPU-MSR-CONTRACT.md` §5, this is **xAPIC only** — 32-bit MMIO
//! registers at 16-byte-aligned offsets, no x2APIC MSR interface. The Version
//! register reports [`APIC_VERSION_VALUE`] (`0x0005_0014`: version `0x14`,
//! max-LVT = 5), so the **six** LVT entries are Timer, Thermal, PerfMon, LINT0,
//! LINT1, Error — **CMCI (0x2F0) is not modeled** (reads 0, drops writes like any
//! reserved register). Writes to read-only or reserved-but-in-range registers are
//! **silently dropped** (deny-ignore-write — the contract's disposition;
//! `vmm-core` logs the drop); only a misaligned or out-of-range offset is a
//! [`LapicError::BadOffset`]. The full register map is cited against the SDM
//! (Vol. 3A §11.x) in `consonance/lapic/IMPLEMENTATION.md`.
//!
//! ## The timer (V-time-driven, the heart of the crate)
//!
//! The timer's input clock is the **frozen core crystal frequency**
//! (`LapicConfig::timer_hz`, from CPUID 0x15), divided by the divide-config
//! register. A write to the Initial Count register stores `initial_count = N`
//! **and** `arm_vns = now_vns` (not just a precomputed deadline) — that is what
//! makes the Current Count round-trip exact for *arbitrary* `timer_hz`. The
//! firing instant is the derived
//! `deadline = arm_vns + ceil(N · divide · 1e9 / timer_hz)` (**ceil**, so the
//! timer never fires before `N` whole ticks elapse); the Current Count register
//! is computed from `now_vns` on read as `N − floor(elapsed_ticks)`, never
//! decremented by a background tick. Mode (one-shot / periodic) comes from the
//! LVT-timer entry; **TSC-deadline mode is held stopped** (R1 masks its CPUID
//! bit). All of this arithmetic is integer-only in `u128` intermediates,
//! saturating to `u64::MAX` / `u32::MAX` — `vtime`'s house style. There is no
//! floating point and no `HashMap`/`HashSet` reaching a snapshot byte or output,
//! so identical inputs yield bit-identical register reads, deadlines, and
//! [`LapicState`].
//!
//! ## Prioritized delivery
//!
//! Interrupt priority class is `vector >> 4`. PPR is derived from TPR and the
//! highest in-service vector per the SDM; [`Lapic::has_deliverable`] is true iff
//! the highest IRR vector's class exceeds PPR's class and the APIC is
//! software-enabled. [`Lapic::take_interrupt`] moves the highest such vector
//! IRR→ISR (raising PPR) and returns it; [`Lapic::eoi`] clears the highest ISR
//! bit (lowering PPR) — LIFO interrupt nesting. When the APIC is
//! software-disabled (SVR bit 8 = 0, the reset state) nothing is deliverable.
//!
//! ## Single vCPU
//!
//! The only IPI destination is self: a fixed-mode ICR write whose shorthand
//! includes self raises the vector locally; all-excluding-self and any non-self
//! destination are no-ops (nowhere to deliver). There is no IOAPIC, no x2APIC,
//! and no inter-processor delivery.

mod device;
mod error;
mod state;

pub use device::{Lapic, LapicConfig};
pub use error::LapicError;
pub use state::{
    APIC_BASE_DEFAULT, APIC_DFR, APIC_EOI, APIC_ESR, APIC_ICR_HIGH, APIC_ICR_LOW, APIC_ID,
    APIC_IRR, APIC_ISR, APIC_LDR, APIC_LVT_ERROR, APIC_LVT_LINT0, APIC_LVT_LINT1, APIC_LVT_PERFMON,
    APIC_LVT_THERMAL, APIC_LVT_TIMER, APIC_MAX_OFFSET, APIC_MMIO_SIZE, APIC_PPR, APIC_SVR,
    APIC_TDCR, APIC_TMCCT, APIC_TMICT, APIC_TMR, APIC_TPR, APIC_VERSION, APIC_VERSION_VALUE,
    LAPIC_STATE_VERSION, LapicState,
};
