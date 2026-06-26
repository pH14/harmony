// SPDX-License-Identifier: AGPL-3.0-or-later
//! Public constants (MMIO base/size, register offsets) and the plain-data
//! snapshot struct [`LapicState`].

/// xAPIC MMIO base address, `0xFEE0_0000` (the architectural power-on default).
///
/// The base is relocatable through `IA32_APIC_BASE`, but that MSR lives in
/// `vmm-core`, not here — this crate is addressed by *offset within the page*
/// (see [`mmio_read`](crate::Lapic::mmio_read)).
pub const APIC_BASE_DEFAULT: u64 = 0xFEE0_0000;

/// Size of the xAPIC MMIO region: one 4 KiB page.
pub const APIC_MMIO_SIZE: usize = 0x1000;

/// Value reported by the Version register (0x30): xAPIC version `0x14`,
/// max-LVT-entry `5` (the six LVT entries Timer/Thermal/PerfMon/LINT0/LINT1/Error,
/// numbered from 0 — **CMCI is not modeled**), giving `(5 << 16) | 0x14`.
pub const APIC_VERSION_VALUE: u32 = 0x0005_0014;

/// Format version of [`LapicState`]. Task 09 (`vm-state`) keys its
/// device-section decoding on this; bump it on any layout change. Version 2
/// added `timer_pending` (distinguishing a still-loaded count from a consumed
/// one-shot); version 3 added `count_at_arm` (the count remaining at the anchor,
/// so a mid-count divide change re-anchors instead of applying retroactively).
pub const LAPIC_STATE_VERSION: u32 = 3;

// --- Register offsets (16-byte aligned, within the 4 KiB page) --------------
// SDM Vol. 3A §11.4.1 "The Local APIC Block Diagram", Table 11-1.

/// Local APIC ID register (read-only here; the single-vCPU ID is frozen).
pub const APIC_ID: u32 = 0x020;
/// Local APIC Version register (read-only, [`APIC_VERSION_VALUE`]).
pub const APIC_VERSION: u32 = 0x030;
/// Task Priority Register.
pub const APIC_TPR: u32 = 0x080;
/// Processor Priority Register (read-only, derived from TPR and ISR).
pub const APIC_PPR: u32 = 0x0A0;
/// End-Of-Interrupt register (write-only).
pub const APIC_EOI: u32 = 0x0B0;
/// Logical Destination Register.
pub const APIC_LDR: u32 = 0x0D0;
/// Destination Format Register.
pub const APIC_DFR: u32 = 0x0E0;
/// Spurious Interrupt Vector Register (bit 8 = APIC software enable).
pub const APIC_SVR: u32 = 0x0F0;
/// In-Service Register, first of 8 32-bit words (0x100..=0x170).
pub const APIC_ISR: u32 = 0x100;
/// Trigger Mode Register, first of 8 32-bit words (0x180..=0x1F0).
pub const APIC_TMR: u32 = 0x180;
/// Interrupt Request Register, first of 8 32-bit words (0x200..=0x270).
pub const APIC_IRR: u32 = 0x200;
/// Error Status Register.
pub const APIC_ESR: u32 = 0x280;
/// Interrupt Command Register, low dword (write triggers a self-IPI).
pub const APIC_ICR_LOW: u32 = 0x300;
/// Interrupt Command Register, high dword (destination field).
pub const APIC_ICR_HIGH: u32 = 0x310;
/// LVT Timer entry.
pub const APIC_LVT_TIMER: u32 = 0x320;
/// LVT Thermal Sensor entry.
pub const APIC_LVT_THERMAL: u32 = 0x330;
/// LVT Performance Monitoring Counters entry.
pub const APIC_LVT_PERFMON: u32 = 0x340;
/// LVT LINT0 entry.
pub const APIC_LVT_LINT0: u32 = 0x350;
/// LVT LINT1 entry.
pub const APIC_LVT_LINT1: u32 = 0x360;
/// LVT Error entry.
pub const APIC_LVT_ERROR: u32 = 0x370;
/// Initial Count register (write arms the timer).
pub const APIC_TMICT: u32 = 0x380;
/// Current Count register (read-only, derived from V-time).
pub const APIC_TMCCT: u32 = 0x390;
/// Divide Configuration register.
pub const APIC_TDCR: u32 = 0x3E0;

/// Largest valid (16-byte aligned) register offset within the page.
pub const APIC_MAX_OFFSET: u32 = 0xFF0;

/// Plain-data, versioned image of a [`Lapic`](crate::Lapic): the full register
/// file plus the timer bookkeeping needed to reproduce it observationally.
///
/// This is the struct task 09 (`vm-state`) embeds verbatim in the `vm_state`
/// blob. Every field is public so it can be serialized field-by-field; with the
/// `serde` feature it additionally derives `Serialize`/`Deserialize`. It holds
/// no `HashMap`/`HashSet` and no floats, so equal [`Lapic`](crate::Lapic) states
/// always produce bit-identical `LapicState` (a determinism requirement).
///
/// The timer is stored as `initial_count` (the last value written to
/// [`APIC_TMICT`]), `count_at_arm` (the count remaining at `timer_arm_vns` — the
/// countdown anchor), `timer_arm_vns` (the V-time of that anchor),
/// `timer_running` (currently counting), and `timer_pending` (the loaded count
/// is not yet consumed). The firing deadline is **derived**
/// (`arm_vns + ceil(count_at_arm·divide·1e9 / timer_hz)`), never stored, so
/// absolute V-time deadlines survive restore unchanged. Tracking `count_at_arm`
/// separately from `initial_count` lets a mid-count divide-config change
/// re-anchor from the *current* remaining count rather than applying the new
/// divisor retroactively. The timer *mode* (one-shot / periodic) is derived from
/// the LVT-timer entry in `lvt`, not duplicated here.
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LapicState {
    /// Snapshot format version; must equal [`LAPIC_STATE_VERSION`] on restore.
    pub version: u32,
    /// APIC ID register value (the APIC ID is in bits 24..=31).
    pub id: u32,
    /// Frozen APIC-timer input frequency in Hz (CPUID 0x15 core crystal). Must
    /// be non-zero; checked on restore.
    pub timer_hz: u64,
    /// Task Priority Register (low byte meaningful).
    pub tpr: u32,
    /// Spurious Interrupt Vector Register (bit 8 = software enable).
    pub svr: u32,
    /// Logical Destination Register.
    pub ldr: u32,
    /// Destination Format Register.
    pub dfr: u32,
    /// Error Status Register.
    pub esr: u32,
    /// Interrupt Command Register, low dword.
    pub icr_low: u32,
    /// Interrupt Command Register, high dword.
    pub icr_high: u32,
    /// Divide Configuration register.
    pub divide_config: u32,
    /// In-Service Register, 256 bits as 8 little-endian-indexed words.
    pub isr: [u32; 8],
    /// Trigger Mode Register, 256 bits as 8 words.
    pub tmr: [u32; 8],
    /// Interrupt Request Register, 256 bits as 8 words.
    pub irr: [u32; 8],
    /// The six LVT entries, in order: Timer, Thermal, PerfMon, LINT0, LINT1,
    /// Error.
    pub lvt: [u32; 6],
    /// Last value written to the Initial Count register (`N`). Retained after a
    /// one-shot expires (the Initial Count register keeps its value), so it
    /// **cannot** by itself indicate whether the timer should re-arm — see
    /// `timer_pending`.
    pub initial_count: u32,
    /// Count remaining at `timer_arm_vns` — the value the countdown (and so the
    /// derived deadline / Current Count) is measured from. Equals
    /// `initial_count` at a fresh arm or periodic reload; a smaller remaining
    /// after a mid-count divide-config change re-anchors the timer. (≤
    /// `initial_count` whenever the timer is running.)
    pub count_at_arm: u32,
    /// V-time (ns) at which the timer was (re-)armed.
    pub timer_arm_vns: u64,
    /// Whether the timer is currently counting toward a deadline. `false` while
    /// masked, software-disabled, or stopped.
    pub timer_running: bool,
    /// Whether the loaded count is still **pending** — armed or waiting to be
    /// (re-)armed by a gating change. Set by a non-zero Initial Count write,
    /// cleared by writing 0 or when a one-shot fires (the count is consumed). A
    /// gating change (unmask/enable) only re-arms while this is `true`, so a
    /// fired one-shot is never resurrected without a fresh Initial Count write.
    pub timer_pending: bool,
}
