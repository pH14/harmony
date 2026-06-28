// SPDX-License-Identifier: AGPL-3.0-or-later
//! The [`Lapic`] state machine: register file, prioritized interrupt delivery,
//! and the V-time-driven LVT timer.
//!
//! All timer arithmetic is integer-only, computed in `u128` intermediates and
//! saturating to `u64::MAX` / `u32::MAX` — `vtime`'s house style. There is no
//! floating point, no map iteration reaching an output, and no clock read: the
//! caller always supplies `now_vns`.

use crate::error::LapicError;
use crate::state::{
    APIC_DFR, APIC_EOI, APIC_ESR, APIC_ICR_HIGH, APIC_ICR_LOW, APIC_ID, APIC_IRR, APIC_ISR,
    APIC_LDR, APIC_LVT_TIMER, APIC_MAX_OFFSET, APIC_PPR, APIC_SVR, APIC_TDCR, APIC_TMCCT,
    APIC_TMICT, APIC_TMR, APIC_TPR, APIC_VERSION, APIC_VERSION_VALUE, LAPIC_STATE_VERSION,
    LapicState,
};

/// Nanoseconds per second — the V-time/timer-tick scaling constant.
const NS_PER_SEC: u128 = 1_000_000_000;

/// Index of the Timer entry within the `lvt` array (Timer, Thermal, PerfMon,
/// LINT0, LINT1, Error).
const LVT_TIMER: usize = 0;

/// Reset value of every LVT entry: masked (bit 16), vector 0.
const LVT_RESET: u32 = 0x0001_0000;

/// LVT mask bit (bit 16): when set, the entry does not deliver.
const LVT_MASK_BIT: u32 = 1 << 16;

/// APIC software-enable bit in the SVR (bit 8).
const SVR_ENABLE_BIT: u32 = 1 << 8;

/// Reset SVR value: spurious vector `0xFF`, software-**disabled** (bit 8 = 0).
const SVR_RESET: u32 = 0x0000_00FF;

/// Writable SVR bits: vector[7:0], software-enable[8], focus-check[9],
/// EOI-broadcast-suppress[12]. Other bits are reserved and read 0.
const SVR_WRITE_MASK: u32 = 0x0000_13FF;

/// Writable ICR-low bits: vector[7:0], delivery-mode[10:8], dest-mode[11],
/// level[14], trigger[15], destination-shorthand[19:18]. The delivery-status
/// bit[12] is read-only and is masked out.
const ICR_LOW_WRITE_MASK: u32 = 0x000C_CFFF;

/// ESR "send illegal vector" bit (bit 5): a fixed-mode IPI was sent with a
/// reserved vector (`< 16`). SDM Vol. 3A §11.5.3.
const ESR_SEND_ILLEGAL_VECTOR: u32 = 1 << 5;

/// Physical-destination broadcast APIC ID (`0xFF`): reaches every LAPIC,
/// including self.
const PHYSICAL_BROADCAST: u32 = 0xFF;

// --- Per-register guest-writable bit masks (the write-mask table) -----------
// For every storage register, exactly the bits the guest may set per the SDM
// (Vol. 3A §11.5/§11.6, Figure 11-8) and the frozen CPU/MSR contract.
// `mmio_write` stores `value & MASK` (DFR additionally forces its reserved bits
// to 1), and `restore` rejects any `LapicState` with bits set outside these
// masks — so no register can hold a reserved bit, whether reached by MMIO or by
// snapshot restore. `SVR_WRITE_MASK` and `ICR_LOW_WRITE_MASK` (above) are part
// of this table.

/// ID register: the 8-bit APIC ID in bits 24..=31 (the only legal bits; the
/// register is read-only, but this also bounds restore validation).
const ID_VALID_MASK: u32 = 0xFF00_0000;
/// Task Priority Register: the 8-bit task priority.
const TPR_WRITE_MASK: u32 = 0x0000_00FF;
/// Logical Destination Register: the 8-bit logical ID in bits 24..=31.
const LDR_WRITE_MASK: u32 = 0xFF00_0000;
/// Destination Format Register: only the 4-bit model in bits 28..=31 is
/// writable.
const DFR_WRITE_MASK: u32 = 0xF000_0000;
/// DFR reserved bits (0..=27), which always read as 1.
const DFR_RESERVED_ONES: u32 = 0x0FFF_FFFF;
/// Error Status Register: the only bit this model ever sets is "send illegal
/// vector" (bit 5); a guest write clears the register.
const ESR_VALID_MASK: u32 = ESR_SEND_ILLEGAL_VECTOR;
/// ICR high: the 8-bit physical destination in bits 24..=31.
const ICR_HIGH_WRITE_MASK: u32 = 0xFF00_0000;
/// Divide Configuration register: only bits [3,1,0] select the divisor. Bit 2 is
/// a **decode-ignored** input — accepted (a write to it is not an error) but
/// *not stored*: masking it off keeps two guests that differ only in TDCR bit 2
/// at the same `divide_config`, so they snapshot/hash identically (a determinism
/// requirement). `divide_value` already ignores bit 2, so the decoded divisor is
/// unchanged whether or not the guest set it.
const TDCR_WRITE_MASK: u32 = 0x0000_000B;

/// LVT Timer writable bits: vector | mask(16) | timer-mode(18:17).
const LVT_TIMER_MASK: u32 = 0x0007_00FF;
/// LVT Thermal / PerfMon writable bits: vector | delivery-mode(10:8) | mask(16).
const LVT_LOCAL_MASK: u32 = 0x0001_07FF;
/// LVT LINT0 / LINT1 writable bits: vector | delivery-mode(10:8) | polarity(13)
/// | trigger(15) | mask(16).
const LVT_LINT_MASK: u32 = 0x0001_A7FF;
/// LVT Error writable bits: vector | mask(16) **only**. The Error LVT has **no**
/// delivery-mode field (SDM Vol. 3A §11.5.1, Figure 11-8), unlike Thermal and
/// PerfMon — sharing their mask would leak reserved bits 8..=10.
const LVT_ERROR_MASK: u32 = 0x0001_00FF;

/// LVT-timer mode field (bits 18:17): one-shot.
const TIMER_ONESHOT: u32 = 0b00;
/// LVT-timer mode field (bits 18:17): periodic.
const TIMER_PERIODIC: u32 = 0b01;

/// Local APIC ID register value at reset is `apic_id << 24`; only bits 24..=31
/// carry the ID.
const ID_SHIFT: u32 = 24;

/// Configuration for constructing a [`Lapic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LapicConfig {
    /// The local APIC ID (placed in bits 24..=31 of the ID register; only the
    /// low 8 bits are significant in xAPIC mode).
    pub apic_id: u32,
    /// Frozen APIC-timer input frequency in Hz — the core crystal clock from
    /// CPUID 0x15 per `docs/CPU-MSR-CONTRACT.md`. The timer counts down at this
    /// rate divided by the divide-config setting. Must be non-zero.
    pub timer_hz: u64,
}

/// A userspace-emulated xAPIC: the register file plus timer bookkeeping.
///
/// Construct with [`Lapic::new`]; drive it with [`Lapic::mmio_read`] /
/// [`Lapic::mmio_write`] (the guest's MMIO accesses), [`Lapic::advance_to`] (the
/// V-time tick), and the delivery methods. Snapshot/restore via
/// [`Lapic::snapshot`] / [`Lapic::restore`]. It never reads a clock — every
/// time-dependent value is a pure function of the `now_vns` the caller passes.
#[derive(Clone, Debug)]
pub struct Lapic {
    /// ID register value (`apic_id << 24`).
    id: u32,
    /// Frozen timer input frequency in Hz; invariant: non-zero.
    timer_hz: u64,
    tpr: u32,
    svr: u32,
    ldr: u32,
    dfr: u32,
    esr: u32,
    icr_low: u32,
    icr_high: u32,
    divide_config: u32,
    isr: [u32; 8],
    tmr: [u32; 8],
    irr: [u32; 8],
    /// LVT entries: Timer, Thermal, PerfMon, LINT0, LINT1, Error.
    lvt: [u32; 6],
    /// Last value written to the Initial Count register (`N`); retained after a
    /// one-shot expires. The TMICT readback and the periodic reload value.
    initial_count: u32,
    /// Count remaining at `timer_arm_vns` — what the countdown/deadline/Current
    /// Count are measured from. `= initial_count` at a fresh arm or periodic
    /// reload; the *current remaining* after a mid-count divide-config re-anchor.
    count_at_arm: u32,
    /// V-time (ns) at which the timer was (re-)armed.
    timer_arm_vns: u64,
    /// Whether the timer is currently counting toward a deadline.
    timer_running: bool,
    /// Whether the loaded count is still pending (not consumed by a one-shot
    /// fire) — the gate for re-arming on an unmask/enable. See `timer_armable`.
    timer_pending: bool,
}

impl Lapic {
    /// Power-on/reset state per the SDM (Vol. 3A §11.4.7.1 "Local APIC State
    /// After Power-Up or Reset"): software-disabled (SVR bit 8 = 0), spurious
    /// vector `0xFF`, all LVT entries masked, IRR/ISR/TMR clear, TPR = 0, DFR
    /// flat (`0xFFFF_FFFF`), timer stopped.
    ///
    /// # Errors
    ///
    /// [`LapicError::InvalidState`] if `cfg.timer_hz == 0` (the timer arithmetic
    /// divides by it).
    pub fn new(cfg: LapicConfig) -> Result<Lapic, LapicError> {
        if cfg.timer_hz == 0 {
            return Err(LapicError::InvalidState);
        }
        Ok(Lapic {
            id: (cfg.apic_id & 0xFF) << ID_SHIFT,
            timer_hz: cfg.timer_hz,
            tpr: 0,
            svr: SVR_RESET,
            ldr: 0,
            dfr: 0xFFFF_FFFF,
            esr: 0,
            icr_low: 0,
            icr_high: 0,
            divide_config: 0,
            isr: [0; 8],
            tmr: [0; 8],
            irr: [0; 8],
            lvt: [LVT_RESET; 6],
            initial_count: 0,
            count_at_arm: 0,
            timer_arm_vns: 0,
            timer_running: false,
            timer_pending: false,
        })
    }

    /// Read a 32-bit register at `offset` (must be 16-byte aligned and in
    /// `0x000..=0xFF0`). `now_vns` lets the Current Count register (0x390)
    /// reflect elapsed V-time; all other reads ignore it. Reads have no side
    /// effects.
    ///
    /// Reads of unimplemented-but-architectural or write-only registers return
    /// `0`.
    ///
    /// # Errors
    ///
    /// [`LapicError::BadOffset`] if the offset is misaligned or out of range.
    pub fn mmio_read(&self, offset: u32, now_vns: u64) -> Result<u32, LapicError> {
        check_offset(offset)?;
        let value = match offset {
            APIC_ID => self.id,
            APIC_VERSION => APIC_VERSION_VALUE,
            APIC_TPR => self.tpr,
            APIC_PPR => self.ppr(),
            APIC_LDR => self.ldr,
            APIC_DFR => self.dfr,
            APIC_SVR => self.svr,
            APIC_ESR => self.esr,
            APIC_ICR_LOW => self.icr_low,
            APIC_ICR_HIGH => self.icr_high,
            APIC_TMICT => self.initial_count,
            APIC_TMCCT => self.current_count(now_vns),
            APIC_TDCR => self.divide_config,
            0x100..=0x170 => self.isr[((offset - APIC_ISR) >> 4) as usize],
            0x180..=0x1F0 => self.tmr[((offset - APIC_TMR) >> 4) as usize],
            0x200..=0x270 => self.irr[((offset - APIC_IRR) >> 4) as usize],
            0x320..=0x370 => self.lvt[((offset - APIC_LVT_TIMER) >> 4) as usize],
            // Write-only (EOI 0xB0), unimplemented-architectural (incl. LVT CMCI
            // 0x2F0, which max-LVT=5 excludes), and reserved-in-range offsets all
            // read 0.
            _ => 0,
        };
        Ok(value)
    }

    /// Write a 32-bit register at `offset`. Writes to read-only or
    /// reserved-but-in-range registers (ID, Version, PPR, Current Count,
    /// ISR/TMR/IRR, CMCI, …) are **silently dropped** (deny-ignore-write — the
    /// CPU/MSR contract's disposition; `vmm-core` logs the drop). Observable
    /// effects (timer arm, self-IPI, EOI) surface through the other methods.
    ///
    /// # Errors
    ///
    /// [`LapicError::BadOffset`] if the offset is misaligned or out of range.
    /// A reserved or read-only *write* in range is never an error.
    pub fn mmio_write(&mut self, offset: u32, value: u32, now_vns: u64) -> Result<(), LapicError> {
        check_offset(offset)?;
        // Every storage register applies its entry from the write-mask table, so
        // a reserved bit can never be stored.
        match offset {
            APIC_TPR => self.tpr = value & TPR_WRITE_MASK,
            APIC_EOI => self.eoi(),
            APIC_LDR => self.ldr = value & LDR_WRITE_MASK,
            // Only the model (bits 28..=31) is writable; reserved bits read as 1.
            // `+` not `|`: the masked value and the reserved-ones are disjoint, so
            // they combine identically — but `|`/`^` are equivalent on disjoint
            // bits (an unkillable mutant), whereas `+`→`-`/`*` are caught.
            APIC_DFR => self.dfr = (value & DFR_WRITE_MASK) + DFR_RESERVED_ONES,
            // The software-enable bit (8) gates the timer: enabling can arm a
            // count loaded while disabled; disabling cancels it.
            APIC_SVR => self.timer_config_write(now_vns, |s| s.svr = value & SVR_WRITE_MASK),
            // Writing the ESR latches/clears the accumulated error state.
            APIC_ESR => self.esr = 0,
            APIC_ICR_LOW => self.write_icr_low(value),
            APIC_ICR_HIGH => self.icr_high = value & ICR_HIGH_WRITE_MASK,
            APIC_TMICT => self.write_initial_count(value, now_vns),
            // The Divide Configuration register is `apic.timer-arm`: a mid-count
            // change must reschedule from the current remaining count, not apply
            // the new divisor retroactively.
            APIC_TDCR => {
                self.timer_config_write(now_vns, |s| s.divide_config = value & TDCR_WRITE_MASK)
            }
            0x320..=0x370 => {
                let idx = ((offset - APIC_LVT_TIMER) >> 4) as usize;
                if idx == LVT_TIMER {
                    // An LVT-timer write is a timer-arm event (mask/mode/vector).
                    self.timer_config_write(now_vns, |s| s.lvt[LVT_TIMER] = value & LVT_TIMER_MASK);
                } else {
                    self.lvt[idx] = value & lvt_write_mask(idx);
                }
            }
            // ID, Version, PPR, Current Count, ISR/TMR/IRR, CMCI (0x2F0), and
            // every other reserved-in-range aligned offset: deny-ignore-write.
            _ => {}
        }
        Ok(())
    }

    /// Absolute V-time (ns) at which the armed timer next expires, or `None` if
    /// the timer is stopped, masked, the APIC is software-disabled, the LVT-timer
    /// is in an unsupported mode (TSC-deadline / reserved), **or the deadline
    /// `arm_vns + period` is unrepresentable in `u64`** (a period beyond ~584
    /// years of V-time). Returning `None` in that last case — rather than
    /// advertising a clamped `u64::MAX` that [`Lapic::advance_to`] will never
    /// fire — keeps a `vtime::TimerQueue` caller from looping on a due-but-never-
    /// firing timer.
    pub fn next_timer_deadline(&self) -> Option<u64> {
        if !self.timer_active() {
            return None;
        }
        let deadline = u128::from(self.timer_arm_vns) + self.period_for(self.count_at_arm);
        u64::try_from(deadline).ok()
    }

    /// Advance V-time to `now_vns`. If an armed, unmasked timer is now due, set
    /// its LVT-timer vector in IRR; re-arm for the next period if periodic, else
    /// stop. Idempotent for a given `now_vns`. Returns `true` if any state
    /// changed (so the caller knows to re-read [`Lapic::next_timer_deadline`] /
    /// [`Lapic::has_deliverable`]).
    pub fn advance_to(&mut self, now_vns: u64) -> bool {
        if !self.timer_active() {
            return false;
        }
        // Fire when the current segment's whole `count_at_arm` ticks have
        // elapsed — decided on the **un-saturated** `u128` span, not `now >=
        // deadline` (a saturating deadline near `u64::MAX` would clamp and
        // re-fire forever, breaking idempotence). A span beyond `u64::MAX` simply
        // never fires (matching `next_timer_deadline`'s `None`).
        let seg_period = self.period_for(self.count_at_arm);
        let elapsed = u128::from(now_vns.saturating_sub(self.timer_arm_vns));
        if elapsed < seg_period {
            return false;
        }
        let vector = self.timer_vector();
        if self.timer_mode() == TIMER_PERIODIC {
            // The current segment (`count_at_arm` ticks, possibly a remainder
            // after a mid-count re-anchor) completes at `arm + seg_period`; the
            // timer then **reloads the full `initial_count`** and fires every full
            // period. Catch up missed full periods closed-form (drift-free) so a
            // `now` that jumps many periods ahead in one tick lands exactly, and a
            // repeat call at the same `now_vns` is a no-op (idempotent).
            let full = self.period_for(self.initial_count); // ≥ 1 (N ≥ 1)
            let after_first = u128::from(self.timer_arm_vns) + seg_period;
            let extra = u128::from(now_vns).saturating_sub(after_first); // now ≥ after_first
            let k = extra / full;
            self.timer_arm_vns = sat_u64(after_first + k * full);
            self.count_at_arm = self.initial_count;
        } else {
            // One-shot: fire once, then stop *and consume the count* — clearing
            // `timer_pending` so a later gating change (SVR/LVT write) cannot
            // re-arm it; only a fresh Initial Count write can. (TSC-deadline /
            // reserved are excluded by `timer_active`.)
            self.timer_running = false;
            self.timer_pending = false;
        }
        // The LVT-timer vector is the guest's; a reserved (<16) vector lands in
        // IRR but is never deliverable (its priority class 0 can never exceed
        // PPR's), so it is harmless rather than an error here.
        set_vec(&mut self.irr, vector);
        true
    }

    /// Raise an edge-triggered interrupt request for `vector` (sets its IRR
    /// bit). The path the LVT timer and self-IPI take internally, and the seam a
    /// future device would use.
    ///
    /// # Errors
    ///
    /// [`LapicError::ReservedVector`] if `vector < 16` (architecturally
    /// reserved).
    pub fn raise(&mut self, vector: u8) -> Result<(), LapicError> {
        if vector < 16 {
            return Err(LapicError::ReservedVector(vector));
        }
        set_vec(&mut self.irr, vector);
        Ok(())
    }

    /// Is there a pending IRR vector whose priority class exceeds the current
    /// PPR's, with the APIC software-enabled? `vmm-core` uses this to decide
    /// whether to request an interrupt window.
    pub fn has_deliverable(&self) -> bool {
        if !self.apic_enabled() {
            return false;
        }
        match highest_vec(&self.irr) {
            Some(v) => priority_class(v) > (self.ppr() >> 4),
            None => false,
        }
    }

    /// The vector [`Lapic::take_interrupt`] would deliver next, **without**
    /// moving it IRR→ISR — the non-mutating sibling of `take_interrupt`. `None`
    /// when nothing is deliverable above PPR or the APIC is software-disabled.
    ///
    /// Under a userspace irqchip the IRR→ISR transition models *interrupt
    /// acceptance*, which actually happens inside the hypervisor on VM-entry — so
    /// `vmm-core` chooses the vector to hand to the backend with `peek_interrupt`
    /// (leaving it pending in IRR), and only calls [`Lapic::take_interrupt`] once
    /// the backend confirms the vector was accepted. That keeps the register file
    /// (and any snapshot taken before acceptance) showing the vector pending in
    /// IRR rather than prematurely in-service.
    pub fn peek_interrupt(&self) -> Option<u8> {
        if !self.apic_enabled() {
            return None;
        }
        let v = highest_vec(&self.irr)?;
        if priority_class(v) <= (self.ppr() >> 4) {
            return None;
        }
        Some(v)
    }

    /// Would the LVT-timer interrupt actually be **delivered** (injected), not
    /// merely fire into the IRR, if it expired now? `true` iff the timer is active
    /// (armed, APIC-enabled, unmasked, supported mode — the same gate as
    /// [`Lapic::next_timer_deadline`]), its vector is a **valid** interrupt vector
    /// (`>= 16` — a reserved vector `< 16` never delivers, SDM §11.5.3), **and**
    /// that vector's priority class outranks the current PPR (so
    /// [`Lapic::peek_interrupt`] would return it once it is in the IRR).
    ///
    /// An armed-but-**undeliverable** timer (reserved vector, or masked by
    /// TPR/PPR) fires into the IRR but is never injected, so it is **not** a real
    /// wake event. `vmm-core`'s idle-`HLT` discriminator uses this — *deliverable*,
    /// not merely *armed* — to avoid treating such a timer as a resumable idle
    /// (which would otherwise warp V-time forever with no wake). It is a pure,
    /// non-mutating predicate (does not fire the timer or touch the IRR).
    pub fn armed_timer_deliverable(&self) -> bool {
        self.timer_active()
            && self.timer_vector() >= 16
            && priority_class(self.timer_vector()) > (self.ppr() >> 4)
    }

    /// Deliver the highest-priority pending vector to the guest: move it
    /// IRR→ISR, (implicitly) raise PPR, and return the vector for
    /// `KVM_INTERRUPT`. `None` if nothing is deliverable above PPR or the APIC
    /// is software-disabled. The caller must only invoke this on confirmed
    /// acceptance (that gate lives in `vmm-core`); [`Lapic::peek_interrupt`] is
    /// the non-mutating pre-acceptance query.
    pub fn take_interrupt(&mut self) -> Option<u8> {
        let v = self.peek_interrupt()?;
        clear_vec(&mut self.irr, v);
        set_vec(&mut self.isr, v);
        Some(v)
    }

    /// End-of-interrupt: clear the highest in-service (ISR) bit, lowering PPR.
    /// Equivalent to a guest write to the EOI register. No-op (not an error) if
    /// ISR is empty.
    pub fn eoi(&mut self) {
        if let Some(v) = highest_vec(&self.isr) {
            clear_vec(&mut self.isr, v);
        }
    }

    /// Plain-data snapshot of the entire register file plus timer state, for
    /// task 09 (`vm-state`). Deterministic: equal `Lapic` states produce equal
    /// [`LapicState`].
    pub fn snapshot(&self) -> LapicState {
        LapicState {
            version: LAPIC_STATE_VERSION,
            id: self.id,
            timer_hz: self.timer_hz,
            tpr: self.tpr,
            svr: self.svr,
            ldr: self.ldr,
            dfr: self.dfr,
            esr: self.esr,
            icr_low: self.icr_low,
            icr_high: self.icr_high,
            divide_config: self.divide_config,
            isr: self.isr,
            tmr: self.tmr,
            irr: self.irr,
            lvt: self.lvt,
            initial_count: self.initial_count,
            count_at_arm: self.count_at_arm,
            timer_arm_vns: self.timer_arm_vns,
            timer_running: self.timer_running,
            timer_pending: self.timer_pending,
        }
    }

    /// Reconstruct a [`Lapic`] from a snapshot, observationally identical to the
    /// one that produced it (same reads at every offset for every `now_vns`,
    /// same deadline, same delivery decisions). Absolute V-time deadlines are
    /// derived, so they survive restore unchanged.
    ///
    /// # Errors
    ///
    /// `restore` is a strict validation boundary: it accepts a `LapicState`
    /// **only if the MMIO write paths could have produced it**, and otherwise
    /// returns [`LapicError::InvalidState`]. The enumerated invariants are:
    ///
    /// - the snapshot `version` is current and `timer_hz != 0`;
    /// - every register holds only its guest-writable / legal bits — no reserved
    ///   bit is set ([`state_bits_canonical`]; this is the same write-mask table
    ///   `mmio_write` enforces);
    /// - the timer bookkeeping is coherent: a pending count is non-zero, and
    ///   `timer_running` equals armability (counting iff the count is pending,
    ///   the APIC enabled, the LVT timer unmasked, and the mode supported). This
    ///   rejects e.g. a fired one-shot marked running, or running-while-masked /
    ///   running-while-disabled.
    ///
    /// A restored LAPIC is observationally identical to the one that produced the
    /// snapshot (same reads at every offset for every `now_vns`, same deadline,
    /// same delivery decisions); absolute V-time deadlines are derived, so they
    /// survive restore unchanged.
    pub fn restore(state: &LapicState) -> Result<Lapic, LapicError> {
        if state.version != LAPIC_STATE_VERSION {
            return Err(LapicError::InvalidState);
        }
        if state.timer_hz == 0 {
            return Err(LapicError::InvalidState);
        }
        if !state_bits_canonical(state) {
            return Err(LapicError::InvalidState);
        }
        let lapic = Lapic {
            id: state.id,
            timer_hz: state.timer_hz,
            tpr: state.tpr,
            svr: state.svr,
            ldr: state.ldr,
            dfr: state.dfr,
            esr: state.esr,
            icr_low: state.icr_low,
            icr_high: state.icr_high,
            divide_config: state.divide_config,
            isr: state.isr,
            tmr: state.tmr,
            irr: state.irr,
            lvt: state.lvt,
            initial_count: state.initial_count,
            count_at_arm: state.count_at_arm,
            timer_arm_vns: state.timer_arm_vns,
            timer_running: state.timer_running,
            timer_pending: state.timer_pending,
        };
        // Timer coherence, checked with the device's own armability predicate so
        // restore and the MMIO paths can never diverge.
        if lapic.timer_pending && lapic.initial_count == 0 {
            return Err(LapicError::InvalidState);
        }
        if lapic.timer_running != lapic.timer_armable() {
            return Err(LapicError::InvalidState);
        }
        // A running timer's anchor count is the full load or a re-anchored
        // remainder — never more than the initial count the MMIO paths can load.
        if lapic.timer_running && lapic.count_at_arm > lapic.initial_count {
            return Err(LapicError::InvalidState);
        }
        Ok(lapic)
    }

    // --- internal helpers ---------------------------------------------------

    /// Is the APIC software-enabled (SVR bit 8)?
    fn apic_enabled(&self) -> bool {
        self.svr & SVR_ENABLE_BIT != 0
    }

    /// Processor Priority Register (SDM Vol. 3A §11.8.3.1): if the TPR class is
    /// at least the highest in-service class, PPR = TPR; otherwise PPR is the
    /// in-service vector's class shifted into bits 7:4.
    fn ppr(&self) -> u32 {
        let tpr = self.tpr & 0xFF;
        let isrv = highest_vec(&self.isr).map(u32::from).unwrap_or(0);
        if (tpr >> 4) >= (isrv >> 4) {
            tpr
        } else {
            isrv & 0xF0
        }
    }

    /// LVT-timer mode field (bits 18:17).
    fn timer_mode(&self) -> u32 {
        (self.lvt[LVT_TIMER] >> 17) & 0b11
    }

    /// LVT-timer interrupt vector (bits 7:0).
    fn timer_vector(&self) -> u8 {
        (self.lvt[LVT_TIMER] & 0xFF) as u8
    }

    /// Is the LVT timer masked (bit 16)?
    fn timer_masked(&self) -> bool {
        self.lvt[LVT_TIMER] & LVT_MASK_BIT != 0
    }

    /// Can the timer produce a deadline / fire? Requires it to be running, the
    /// APIC enabled, the LVT timer unmasked, and the mode supported (one-shot or
    /// periodic — TSC-deadline and the reserved encoding are held stopped).
    fn timer_active(&self) -> bool {
        self.timer_running
            && self.apic_enabled()
            && !self.timer_masked()
            && matches!(self.timer_mode(), TIMER_ONESHOT | TIMER_PERIODIC)
    }

    /// V-time to count down `count` ticks at the current divisor, **un-saturated**:
    /// `ceil(count · divide · 1e9 / timer_hz)` in `u128`. **Ceil** so the timer
    /// never fires before `count` whole ticks have elapsed. `timer_hz` is
    /// non-zero by construction, so the division never traps; the product
    /// `count · divide · 1e9 ≤ u32::MAX · 128 · 1e9 < 2^128`, so the multiply
    /// cannot overflow `u128`. The firing/deadline logic uses this exact value
    /// (not a saturated one) so a span larger than `u64::MAX` is correctly
    /// treated as unreachable rather than clamped to a deadline that never fires.
    fn period_for(&self, count: u32) -> u128 {
        let divide = divide_value(self.divide_config);
        let numer = u128::from(count) * u128::from(divide) * NS_PER_SEC;
        numer.div_ceil(u128::from(self.timer_hz))
    }

    /// Ticks elapsed over `delta` V-time at the current divisor:
    /// `floor(delta · timer_hz / (divide · 1e9))`, saturating to `u32::MAX`.
    fn elapsed_ticks(&self, delta: u64) -> u32 {
        let divide = divide_value(self.divide_config);
        let ticks =
            (u128::from(delta) * u128::from(self.timer_hz)) / (u128::from(divide) * NS_PER_SEC);
        sat_u32(ticks)
    }

    /// Count remaining for a *running* timer at `now_vns`:
    /// `count_at_arm - floor((now - arm) ticks)`, saturating to 0. Measured from
    /// the anchor (`count_at_arm`, `timer_arm_vns`), so it round-trips exactly:
    /// at `now == arm_vns` it is exactly `count_at_arm`.
    fn remaining_at(&self, now_vns: u64) -> u32 {
        let elapsed = now_vns.saturating_sub(self.timer_arm_vns);
        self.count_at_arm
            .saturating_sub(self.elapsed_ticks(elapsed))
    }

    /// Current Count register value at `now_vns`: the remaining count when
    /// running, else 0 (a stopped/masked/disabled timer reads 0).
    fn current_count(&self, now_vns: u64) -> u32 {
        if self.timer_running {
            self.remaining_at(now_vns)
        } else {
            0
        }
    }

    /// Whether the timer should currently be armed and counting: a **pending**
    /// loaded count, the APIC software-enabled, the LVT timer unmasked, and a
    /// supported mode (one-shot or periodic). Keying on `timer_pending` rather
    /// than `initial_count != 0` is what stops a fired one-shot (whose count
    /// register still reads `N`) from being resurrected by a later gating change.
    fn timer_armable(&self) -> bool {
        self.timer_pending
            && self.apic_enabled()
            && !self.timer_masked()
            && matches!(self.timer_mode(), TIMER_ONESHOT | TIMER_PERIODIC)
    }

    /// Apply a write to the Initial Count register (`APIC_TMICT`). A TMICT write
    /// is a **fresh arm**: it loads the full new count and (re)starts the
    /// countdown from `now_vns` if [armable](Self::timer_armable); writing 0
    /// clears the pending count and disarms. A masked/disabled/TSC-deadline timer
    /// stays pending-but-stopped and a later unmask/enable/mode-change re-arms it.
    fn write_initial_count(&mut self, value: u32, now_vns: u64) {
        self.initial_count = value;
        self.timer_pending = value != 0;
        // `None` ⇒ fresh arm: load the full `initial_count` at `now`.
        self.retime(now_vns, None, divide_value(self.divide_config));
    }

    /// The remaining count of a running timer at `now_vns`, or `None` if not
    /// running — the "prior remaining" captured before a timer-affecting write.
    fn running_remaining(&self, now_vns: u64) -> Option<u32> {
        self.timer_running.then(|| self.remaining_at(now_vns))
    }

    /// Apply a timer-affecting register change (`apply`) and then re-time through
    /// the **single** [`retime`](Self::retime) path, so no register change ever
    /// applies retroactively or loses the deadline. Captures the current remaining
    /// count and the current divisor *before* the change (so `retime` can decide
    /// whether to re-anchor).
    fn timer_config_write(&mut self, now_vns: u64, apply: impl FnOnce(&mut Self)) {
        let prior_remaining = self.running_remaining(now_vns);
        let old_divide = divide_value(self.divide_config);
        apply(self);
        self.retime(now_vns, prior_remaining, old_divide);
    }

    /// **The one re-arm path.** Re-establishes `(count_at_arm, timer_arm_vns,
    /// timer_running)` after any timer-affecting change, given the remaining count
    /// captured *before* the change (`Some` iff it was running) and the divisor in
    /// effect *before* the change. The cases:
    ///
    /// - **not armable now** (masked / disabled / unsupported mode / no pending
    ///   count): cancel — `timer_running = false`. No stale deadline lingers.
    /// - **was running, divisor changed** (a mid-count TDCR write): re-anchor from
    ///   the current remaining at `now`, so the new rate applies only going
    ///   forward — never retroactively, never firing in the past.
    /// - **was running, divisor unchanged** (a mask→still-unmasked vector/mode
    ///   change): keep the anchor — the deadline is preserved exactly, with no
    ///   rounding drift.
    /// - **fresh arm** (a TMICT load, or a stopped→armable unmask/enable): load
    ///   the full `initial_count` and count from `now`.
    fn retime(&mut self, now_vns: u64, prior_remaining: Option<u32>, old_divide: u64) {
        if !self.timer_armable() {
            self.timer_running = false;
            return;
        }
        match prior_remaining {
            Some(remaining) if divide_value(self.divide_config) != old_divide => {
                self.count_at_arm = remaining;
                self.timer_arm_vns = now_vns;
            }
            Some(_) => {} // running, no rate change: keep the anchor (exact deadline)
            None => {
                self.count_at_arm = self.initial_count;
                self.timer_arm_vns = now_vns;
            }
        }
        self.timer_running = true;
    }

    /// Apply a write to ICR-low. A fixed-delivery-mode IPI that targets self
    /// raises the vector on this (the only) LAPIC. "Targets self" means: the
    /// destination shorthand is `01` (self) or `10` (all-including-self); **or**
    /// shorthand `00` (no shorthand) with a *physical* destination (ICR-high
    /// bits 24..=31) equal to our APIC ID — the common `0 == 0` case — or the
    /// physical broadcast `0xFF`. Shorthand `11` (all-excluding-self), a
    /// non-matching physical destination, and logical-mode destinations (not
    /// modeled — single vCPU) have nowhere to go and are no-ops. Non-fixed modes
    /// (NMI/INIT/SIPI/…) are not modeled — those are `vmm-core`'s to issue. A
    /// fixed self-IPI with a reserved vector (`< 16`) sets the ESR
    /// "send illegal vector" bit instead of delivering.
    fn write_icr_low(&mut self, value: u32) {
        self.icr_low = value & ICR_LOW_WRITE_MASK;
        let delivery_mode = (value >> 8) & 0b111;
        let shorthand = (value >> 18) & 0b11;
        let physical_dest = (value >> 11) & 1 == 0;
        let vector = (value & 0xFF) as u8;

        let self_target = match shorthand {
            0b01 | 0b10 => true, // self, all-including-self
            0b00 => {
                // No shorthand: honor the ICR-high destination. Physical mode
                // hits self iff the destination equals our APIC ID or is the
                // physical broadcast; logical mode is not modeled here.
                let dest = (self.icr_high >> 24) & 0xFF;
                let apic_id = (self.id >> 24) & 0xFF;
                physical_dest && (dest == apic_id || dest == PHYSICAL_BROADCAST)
            }
            _ => false, // 0b11 = all-excluding-self -> nowhere
        };

        if delivery_mode == 0b000 && self_target {
            if vector >= 16 {
                set_vec(&mut self.irr, vector);
            } else {
                self.esr |= ESR_SEND_ILLEGAL_VECTOR;
            }
        }
    }
}

// --- free helpers -----------------------------------------------------------

/// Validate an MMIO offset: must be 16-byte aligned and within `0x000..=0xFF0`.
fn check_offset(offset: u32) -> Result<(), LapicError> {
    if offset & 0xF != 0 || offset > APIC_MAX_OFFSET {
        return Err(LapicError::BadOffset(offset));
    }
    Ok(())
}

/// Decode the divide-config register into its divisor. All 8 encodings are
/// legal (SDM Vol. 3A §11.5.4): bits [3,1,0] select the divisor (bit 2 ignored).
/// `0b111` is ÷1; otherwise `0b000..=0b110` are ÷2, ÷4, …, ÷128.
fn divide_value(tdcr: u32) -> u64 {
    // The selector packs TDCR bit 3 into position 2 and bits [1:0] into [1:0].
    // Those fields are disjoint, so `+` combines them exactly as `|` would —
    // and, unlike `|`/`^` (equivalent on disjoint bits), it stays
    // mutation-testable: a `+`→`-`/`*` mutant changes the divisor and is caught.
    let sel = ((tdcr & 0b1000) >> 1) + (tdcr & 0b11);
    if sel == 0b111 { 1 } else { 2u64 << sel }
}

/// Writable bits of the LVT entry at `index` (Timer 0, Thermal 1, PerfMon 2,
/// LINT0 3, LINT1 4, Error 5) — the single source of truth used by both
/// `mmio_write` and `restore`'s reserved-bit check. The read-only
/// delivery-status (bit 12) and remote-IRR (bit 14) bits are never writable.
fn lvt_write_mask(index: usize) -> u32 {
    match index {
        0 => LVT_TIMER_MASK,     // Timer: vector | mask | mode
        1 | 2 => LVT_LOCAL_MASK, // Thermal, PerfMon: vector | delivery-mode | mask
        3 | 4 => LVT_LINT_MASK,  // LINT0, LINT1: + polarity | trigger
        _ => LVT_ERROR_MASK,     // Error (index 5): vector | mask only (no delivery-mode)
    }
}

/// Whether every register in `state` holds only its guest-writable / legal bits
/// — i.e. the state is bit-reachable through the masked MMIO write paths. The
/// ISR/TMR/IRR vector bitmaps and `initial_count`/`timer_arm_vns` admit any
/// value (every vector and count is representable) and so are unconstrained.
fn state_bits_canonical(state: &LapicState) -> bool {
    let registers_ok = state.id & !ID_VALID_MASK == 0
        && state.tpr & !TPR_WRITE_MASK == 0
        && state.svr & !SVR_WRITE_MASK == 0
        && state.ldr & !LDR_WRITE_MASK == 0
        // DFR's reserved bits (0..=27) always read 1; bits 28..=31 are the
        // writable model (any value), so that lower-bits check is the only
        // constraint.
        && state.dfr & DFR_RESERVED_ONES == DFR_RESERVED_ONES
        && state.esr & !ESR_VALID_MASK == 0
        && state.icr_low & !ICR_LOW_WRITE_MASK == 0
        && state.icr_high & !ICR_HIGH_WRITE_MASK == 0
        && state.divide_config & !TDCR_WRITE_MASK == 0;
    // Each LVT entry against its own writable mask (Error excludes delivery-mode).
    let lvt_ok = state.lvt[0] & !lvt_write_mask(0) == 0
        && state.lvt[1] & !lvt_write_mask(1) == 0
        && state.lvt[2] & !lvt_write_mask(2) == 0
        && state.lvt[3] & !lvt_write_mask(3) == 0
        && state.lvt[4] & !lvt_write_mask(4) == 0
        && state.lvt[5] & !lvt_write_mask(5) == 0;
    registers_ok && lvt_ok
}

/// Priority class of a vector: its high nibble (`vector >> 4`).
fn priority_class(vector: u8) -> u32 {
    u32::from(vector) >> 4
}

/// Set the IRR/ISR-style bit for `vector` in a 256-bit `[u32; 8]` register.
fn set_vec(bits: &mut [u32; 8], vector: u8) {
    bits[(vector >> 5) as usize] |= 1u32 << (vector & 31);
}

/// Clear the bit for `vector`.
fn clear_vec(bits: &mut [u32; 8], vector: u8) {
    bits[(vector >> 5) as usize] &= !(1u32 << (vector & 31));
}

/// The highest vector set in a 256-bit `[u32; 8]` register, or `None` if empty.
/// Higher vector numbers win, matching xAPIC priority resolution.
fn highest_vec(bits: &[u32; 8]) -> Option<u8> {
    let mut word = 8;
    while word > 0 {
        word -= 1;
        if bits[word] != 0 {
            let bit = 31 - bits[word].leading_zeros();
            return Some((word as u32 * 32 + bit) as u8);
        }
    }
    None
}

/// Saturate a `u128` intermediate to `u64` (the crate-wide overflow rule).
fn sat_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Saturate a `u128` tick count to `u32` (Current Count is a 32-bit register).
fn sat_u32(value: u128) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// Formal proof harnesses (bounded model checking via Kani); compiled only under
/// `cargo kani`. Declared as a child of `device` so `use super::*` reaches the
/// private helpers it verifies. See `IMPLEMENTATION.md` ("Formal proofs (Kani)").
#[cfg(kani)]
#[path = "device_proofs.rs"]
mod proofs;

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled(timer_hz: u64) -> Lapic {
        let mut l = Lapic::new(LapicConfig {
            apic_id: 0,
            timer_hz,
        })
        .expect("valid config");
        // Software-enable the APIC, keeping the reset spurious vector.
        l.mmio_write(APIC_SVR, SVR_RESET | SVR_ENABLE_BIT, 0)
            .expect("svr write");
        l
    }

    #[test]
    fn rejects_zero_timer_hz() {
        let err = Lapic::new(LapicConfig {
            apic_id: 0,
            timer_hz: 0,
        })
        .unwrap_err();
        assert_eq!(err, LapicError::InvalidState);
    }

    #[test]
    fn bad_offsets_rejected() {
        let l = enabled(25_000_000);
        assert_eq!(l.mmio_read(0x004, 0), Err(LapicError::BadOffset(0x004))); // misaligned
        assert_eq!(l.mmio_read(0x1000, 0), Err(LapicError::BadOffset(0x1000))); // out of range
        assert_eq!(
            l.mmio_read(APIC_MAX_OFFSET + 0x10, 0),
            Err(LapicError::BadOffset(APIC_MAX_OFFSET + 0x10))
        );
        // The last in-range aligned offset is fine.
        assert_eq!(l.mmio_read(APIC_MAX_OFFSET, 0), Ok(0));
    }

    #[test]
    fn version_register_is_fixed() {
        let l = enabled(25_000_000);
        assert_eq!(l.mmio_read(APIC_VERSION, 0), Ok(APIC_VERSION_VALUE));
    }

    #[test]
    fn armed_timer_deliverable_gates_on_active_vector_and_priority() {
        // The idle-HLT discriminator (vmm-core task 52) relies on this: a timer is a real
        // wake event only if it is active, has a valid vector (>= 16), and outranks PPR.
        let mut l = enabled(24_000_000);
        // Unarmed → not deliverable.
        assert!(!l.armed_timer_deliverable(), "no timer armed");

        // Armed one-shot, vector 0x40 (class 4), TPR 0 → deliverable.
        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert!(l.armed_timer_deliverable(), "armed, valid vector, low TPR");

        // Vector exactly 16 — the lowest deliverable vector (boundary of `>= 16`).
        l.mmio_write(APIC_LVT_TIMER, 16, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert!(
            l.armed_timer_deliverable(),
            "vector 16 is deliverable (>= 16)"
        );

        // Vector 15 — reserved (< 16): never deliverable however high its arming.
        l.mmio_write(APIC_LVT_TIMER, 15, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert!(
            !l.armed_timer_deliverable(),
            "reserved vector < 16 never delivers"
        );

        // Vector 0x40 (class 4) with TPR class == 4 → masked (the comparison is strict `>`).
        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        l.mmio_write(APIC_TPR, 0x40, 0).unwrap();
        assert!(
            !l.armed_timer_deliverable(),
            "equal priority class is not deliverable"
        );
        // TPR class 3 < vector class 4 → deliverable again.
        l.mmio_write(APIC_TPR, 0x30, 0).unwrap();
        assert!(
            l.armed_timer_deliverable(),
            "vector class outranks TPR class"
        );

        // A masked timer (valid vector, low TPR) is inactive → not deliverable. Isolates
        // the `timer_active()` term (vector/priority would otherwise pass).
        l.mmio_write(APIC_TPR, 0, 0).unwrap();
        l.mmio_write(APIC_LVT_TIMER, 0x40 | LVT_MASK_BIT, 0)
            .unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert!(
            !l.armed_timer_deliverable(),
            "a masked (inactive) timer is never deliverable"
        );
    }

    #[test]
    fn deny_ignore_write_to_readonly() {
        let mut l = enabled(25_000_000);
        // Writing a read-only register is dropped, not an error.
        assert_eq!(l.mmio_write(APIC_VERSION, 0xDEAD_BEEF, 0), Ok(()));
        assert_eq!(l.mmio_read(APIC_VERSION, 0), Ok(APIC_VERSION_VALUE));
        // PPR / Current Count / IRR are read-only too.
        assert_eq!(l.mmio_write(APIC_PPR, 0xFF, 0), Ok(()));
        assert_eq!(l.mmio_write(APIC_TMCCT, 0xFF, 0), Ok(()));
        assert_eq!(l.mmio_write(APIC_IRR, 0xFF, 0), Ok(()));
        // CMCI (0x2F0) is not modeled: reads 0, drops writes.
        assert_eq!(l.mmio_read(0x2F0, 0), Ok(0));
        assert_eq!(l.mmio_write(0x2F0, 0xFF, 0), Ok(()));
        assert_eq!(l.mmio_read(0x2F0, 0), Ok(0));
    }

    #[test]
    fn reserved_vector_raise_errors() {
        let mut l = enabled(25_000_000);
        assert_eq!(l.raise(15), Err(LapicError::ReservedVector(15)));
        assert_eq!(l.raise(0), Err(LapicError::ReservedVector(0)));
        assert_eq!(l.raise(16), Ok(()));
    }

    #[test]
    fn all_divide_encodings_legal() {
        for tdcr in 0u32..=0xF {
            let mut l = enabled(25_000_000);
            assert_eq!(l.mmio_write(APIC_TDCR, tdcr, 0), Ok(()));
            // Bit 2 is decode-ignored and not stored, so the readback masks to
            // bits [3,1,0] (0xB), never the raw 0xF.
            assert_eq!(l.mmio_read(APIC_TDCR, 0), Ok(tdcr & 0xB));
        }
        // Spot-check the decoded divisors. The divisor selector is TDCR bits
        // [3,1,0] (bit 2 ignored); ÷1 is the all-ones selector 0b1011.
        assert_eq!(divide_value(0b0000), 2);
        assert_eq!(divide_value(0b0100), 2); // bit 2 ignored
        assert_eq!(divide_value(0b0001), 4);
        assert_eq!(divide_value(0b0011), 16);
        assert_eq!(divide_value(0b1000), 32);
        assert_eq!(divide_value(0b1010), 128);
        assert_eq!(divide_value(0b1011), 1); // selector 0b111 -> ÷1
        assert_eq!(divide_value(0b1111), 1); // bit 2 ignored -> still ÷1
    }

    #[test]
    fn tdcr_bit2_dropped_not_stored() {
        // The Divide-Config register's bit 2 is decode-ignored — the divisor is
        // bits [3,1,0]. A guest write to it is accepted, but storing the bit
        // would let two behaviorally-identical guests (one wrote TDCR bit 2, one
        // didn't) snapshot to *different* `divide_config` and so hash
        // differently: a determinism gap. The bit is masked off at storage, so
        // the readback, the divide behavior, and the snapshot (what task 09
        // hashes) all match the bit-2-clear write. Use the 24 MHz non-dividing
        // crystal so any leak would also perturb the timer arithmetic.
        for base in 0u32..=0xF {
            let with_bit2 = base | 0b100;
            let without = base & !0b100;

            let mut a = enabled(24_000_000);
            let mut b = enabled(24_000_000);
            a.mmio_write(APIC_TDCR, with_bit2, 0).unwrap();
            b.mmio_write(APIC_TDCR, without, 0).unwrap();

            // The readback masks bit 2 off; both LAPICs read back the same value.
            assert_eq!(a.mmio_read(APIC_TDCR, 0), Ok(without));
            assert_eq!(a.mmio_read(APIC_TDCR, 0), b.mmio_read(APIC_TDCR, 0));

            // Identical divide behavior: same deadline and same count decay after
            // arming the same one-shot.
            a.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
            b.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
            a.mmio_write(APIC_TMICT, 1000, 0).unwrap();
            b.mmio_write(APIC_TMICT, 1000, 0).unwrap();
            assert_eq!(a.next_timer_deadline(), b.next_timer_deadline());
            assert_eq!(a.mmio_read(APIC_TMCCT, 1234), b.mmio_read(APIC_TMCCT, 1234));

            // The snapshot is bit-identical — no determinism gap downstream.
            assert_eq!(a.snapshot(), b.snapshot());
        }

        // The restore-validation half: a stored `divide_config` with bit 2 set is
        // unreachable through the masked write path, so `restore` rejects it.
        let mut state = enabled(24_000_000).snapshot();
        state.divide_config |= 0b100;
        assert_eq!(
            Lapic::restore(&state).unwrap_err(),
            LapicError::InvalidState
        );
    }

    #[test]
    fn self_ipi_raises_vector() {
        let mut l = enabled(25_000_000);
        // Fixed mode (000), self shorthand (01), vector 0x40.
        let icr = 0x40 | (0b01 << 18);
        l.mmio_write(APIC_ICR_LOW, icr, 0).expect("icr write");
        assert!(l.has_deliverable());
        assert_eq!(l.take_interrupt(), Some(0x40));
    }

    #[test]
    fn all_including_self_ipi_raises_vector() {
        let mut l = enabled(25_000_000);
        let icr = 0x50 | (0b10 << 18); // all-incl-self
        l.mmio_write(APIC_ICR_LOW, icr, 0).expect("icr write");
        assert_eq!(l.take_interrupt(), Some(0x50));
    }

    #[test]
    fn all_excluding_self_ipi_is_noop() {
        let mut l = enabled(25_000_000);
        let icr = 0x60 | (0b11 << 18); // all-excl-self -> nowhere
        l.mmio_write(APIC_ICR_LOW, icr, 0).expect("icr write");
        assert!(!l.has_deliverable());
        assert_eq!(l.take_interrupt(), None);
    }

    #[test]
    fn physical_self_ipi_matches_apic_id() {
        let mut l = enabled(25_000_000); // apic_id 0
        // Shorthand 00, physical mode, destination 0 == our APIC ID -> self-IPI.
        l.mmio_write(APIC_ICR_HIGH, 0, 0).expect("icr-high");
        l.mmio_write(APIC_ICR_LOW, 0x60, 0).expect("icr-low"); // vector 0x60
        assert_eq!(l.take_interrupt(), Some(0x60));

        // Physical broadcast 0xFF also reaches self.
        let mut b = enabled(25_000_000);
        b.mmio_write(APIC_ICR_HIGH, PHYSICAL_BROADCAST << 24, 0)
            .unwrap();
        b.mmio_write(APIC_ICR_LOW, 0x61, 0).unwrap();
        assert_eq!(b.take_interrupt(), Some(0x61));
    }

    #[test]
    fn physical_non_matching_destination_is_noop() {
        let mut l = enabled(25_000_000); // apic_id 0
        // Physical destination 5 != our APIC ID 0 -> nowhere to deliver.
        l.mmio_write(APIC_ICR_HIGH, 5 << 24, 0).expect("icr-high");
        l.mmio_write(APIC_ICR_LOW, 0x60, 0).expect("icr-low");
        assert_eq!(l.take_interrupt(), None);
    }

    #[test]
    fn reserved_vector_self_ipi_sets_esr() {
        let mut l = enabled(25_000_000);
        let icr = 0x0F | (0b01 << 18); // vector 15 (<16), self shorthand
        l.mmio_write(APIC_ICR_LOW, icr, 0).expect("icr write");
        // Not delivered, but the illegal-vector error is recorded in the ESR.
        // Assert the literal bit value (0x20 = 1 << 5), not the named constant,
        // so a mutation of the constant diverges from the expectation.
        assert!(!l.has_deliverable());
        assert_eq!(l.mmio_read(APIC_ESR, 0), Ok(0x20));
        // A write to the ESR clears the accumulated error state.
        l.mmio_write(APIC_ESR, 0, 0).expect("esr write");
        assert_eq!(l.mmio_read(APIC_ESR, 0), Ok(0));
    }

    #[test]
    fn non_fixed_delivery_mode_self_ipi_is_noop() {
        let mut l = enabled(25_000_000);
        // NMI delivery mode (0b100 in bits 10:8) with the self shorthand: only
        // *fixed* mode delivers here; non-fixed modes are vmm-core's to issue.
        let icr = 0x40 | (0b100 << 8) | (0b01 << 18);
        l.mmio_write(APIC_ICR_LOW, icr, 0).expect("icr write");
        assert!(!l.has_deliverable());
        assert_eq!(l.take_interrupt(), None);
    }

    #[test]
    fn logical_mode_no_shorthand_ipi_is_noop() {
        let mut l = enabled(25_000_000);
        // Logical destination mode (bit 11) with no shorthand is not modeled
        // (single vCPU) — a no-op even when the destination would match.
        l.mmio_write(APIC_ICR_HIGH, 0, 0).expect("icr-high");
        let icr = 0x40 | (1 << 11); // logical dest mode, shorthand 00
        l.mmio_write(APIC_ICR_LOW, icr, 0).expect("icr-low");
        assert_eq!(l.take_interrupt(), None);
    }

    #[test]
    fn physical_self_ipi_matches_nonzero_apic_id() {
        // A non-zero APIC ID exercises the destination/ID extraction: a physical
        // IPI delivers iff the destination equals the (non-zero) APIC ID.
        let mut l = Lapic::new(LapicConfig {
            apic_id: 0x12,
            timer_hz: 25_000_000,
        })
        .unwrap();
        l.mmio_write(APIC_SVR, SVR_RESET | SVR_ENABLE_BIT, 0)
            .unwrap();
        // Matching physical destination 0x12 -> self-IPI delivers.
        l.mmio_write(APIC_ICR_HIGH, 0x12 << 24, 0).unwrap();
        l.mmio_write(APIC_ICR_LOW, 0x60, 0).unwrap();
        assert_eq!(l.take_interrupt(), Some(0x60));
        // Non-matching destination 0x12 != 0x34 -> no delivery.
        l.mmio_write(APIC_ICR_HIGH, 0x34 << 24, 0).unwrap();
        l.mmio_write(APIC_ICR_LOW, 0x61, 0).unwrap();
        assert_eq!(l.take_interrupt(), None);
    }

    #[test]
    fn software_disabled_blocks_delivery() {
        let mut l = Lapic::new(LapicConfig {
            apic_id: 0,
            timer_hz: 25_000_000,
        })
        .unwrap();
        // APIC software-disabled (reset). raise() still sets IRR but nothing is
        // deliverable.
        l.raise(0x40).unwrap();
        assert!(!l.has_deliverable());
        assert_eq!(l.take_interrupt(), None);
    }

    #[test]
    fn esr_write_clears() {
        let mut l = enabled(25_000_000);
        assert_eq!(l.mmio_read(APIC_ESR, 0), Ok(0));
        l.mmio_write(APIC_ESR, 0x55, 0).expect("esr write");
        assert_eq!(l.mmio_read(APIC_ESR, 0), Ok(0));
    }

    #[test]
    fn eoi_on_empty_is_noop() {
        let mut l = enabled(25_000_000);
        l.eoi(); // no panic, no state change
        assert_eq!(l.take_interrupt(), None);
    }

    #[test]
    fn id_register_reflects_config_and_is_readonly() {
        let mut l = Lapic::new(LapicConfig {
            apic_id: 3,
            timer_hz: 25_000_000,
        })
        .unwrap();
        assert_eq!(l.mmio_read(APIC_ID, 0), Ok(3 << 24));
        // Read-only: write dropped.
        l.mmio_write(APIC_ID, 7 << 24, 0).unwrap();
        assert_eq!(l.mmio_read(APIC_ID, 0), Ok(3 << 24));
    }

    #[test]
    fn stateful_registers_round_trip_through_mmio() {
        // Each writable register must store the masked value and read it straight
        // back — the register-file contract `vmm-core` and snapshots rely on.
        // Writing all-ones exercises every per-register write mask; the exact
        // readback constrains both the masking and the read arm (so a mutated
        // `value & MASK` or read dispatch is killed, not silently survived).
        use crate::state::{
            APIC_LVT_ERROR, APIC_LVT_LINT0, APIC_LVT_LINT1, APIC_LVT_PERFMON, APIC_LVT_THERMAL,
        };
        let cases: &[(u32, u32, u32)] = &[
            (APIC_TPR, 0xFFFF_FFFF, 0x0000_00FF),
            (APIC_LDR, 0xFFFF_FFFF, 0xFF00_0000),
            // Low bits set so the readback distinguishes OR (reserved bits
            // forced to 1) from XOR: 0x0F | 0x0FFF_FFFF == 0x0FFF_FFFF.
            (APIC_DFR, 0x0000_000F, 0x0FFF_FFFF),
            (APIC_SVR, 0xFFFF_FFFF, 0x0000_13FF),
            (APIC_ICR_LOW, 0xFFFF_FFFF, 0x000C_CFFF),
            (APIC_ICR_HIGH, 0xFFFF_FFFF, 0xFF00_0000),
            (APIC_TDCR, 0xFFFF_FFFF, 0x0000_000B), // bit 2 decode-ignored, dropped at storage
            (APIC_LVT_TIMER, 0xFFFF_FFFF, 0x0007_00FF),
            (APIC_LVT_THERMAL, 0xFFFF_FFFF, 0x0001_07FF),
            (APIC_LVT_PERFMON, 0xFFFF_FFFF, 0x0001_07FF),
            (APIC_LVT_LINT0, 0xFFFF_FFFF, 0x0001_A7FF),
            (APIC_LVT_LINT1, 0xFFFF_FFFF, 0x0001_A7FF),
            // Error LVT has NO delivery-mode field: only vector + mask (bits
            // 8..=10 must read 0, unlike Thermal/PerfMon).
            (APIC_LVT_ERROR, 0xFFFF_FFFF, 0x0001_00FF),
            (APIC_TMICT, 0x1234_5678, 0x1234_5678),
        ];
        for &(offset, write, expect) in cases {
            let mut l = enabled(25_000_000);
            // The register reads its reset value before the write and the masked
            // written value after — proving the readback is genuinely stateful,
            // while the literal `expect` pins the write mask and the read dispatch
            // (a mutated mask, write arm, or read arm is killed, not survived).
            let before = l.mmio_read(offset, 0).expect("read");
            assert_ne!(before, expect, "offset {offset:#x} already reads `expect`");
            l.mmio_write(offset, write, 0).expect("write");
            assert_eq!(l.mmio_read(offset, 0), Ok(expect), "offset {offset:#x}");
        }
    }

    #[test]
    fn periodic_advance_idempotent_at_saturated_vtime() {
        // Regression (PR #38): a periodic timer armed just below u64::MAX has a
        // deadline that saturates to u64::MAX; firing on `now >= deadline` would
        // re-deliver forever because `arm_vns` never advances. With the
        // elapsed-based gate, fewer-than-one-period elapsed means no fire.
        let mut l = enabled(25_000_000); // TDCR reset = ÷2
        l.mmio_write(APIC_LVT_TIMER, 0x40 | (TIMER_PERIODIC << 17), 0)
            .unwrap(); // unmasked, periodic, vector 0x40
        let arm = u64::MAX - 10;
        l.mmio_write(APIC_TMICT, 1_000_000, arm).unwrap(); // period = 80_000_000 ns
        // Only 10 ns elapsed at u64::MAX — far less than one period: no fire.
        assert!(!l.advance_to(u64::MAX));
        assert!(!l.has_deliverable());
        // And a repeat at the same V-time stays a no-op.
        let snap = l.snapshot();
        assert!(!l.advance_to(u64::MAX));
        assert_eq!(l.snapshot(), snap);
    }

    #[test]
    fn fired_oneshot_not_rearmed_by_gating_writes() {
        // PR #38 final pass: a fired one-shot must stay disarmed until Initial
        // Count is written again. The count register still reads N (retained),
        // but it is *consumed* — an SVR/LVT rewrite that leaves the timer
        // enabled+unmasked must NOT resurrect it into a spurious second fire.
        let mut l = enabled(25_000_000); // ÷2
        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap(); // unmasked one-shot, vector 0x40
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap(); // arm at t=0; period = 80_000 ns
        let deadline = l.next_timer_deadline().expect("armed");
        assert!(l.advance_to(deadline)); // fires once
        assert_eq!(l.take_interrupt(), Some(0x40));
        l.eoi();
        assert_eq!(l.next_timer_deadline(), None); // one-shot stopped
        assert_eq!(l.mmio_read(APIC_TMICT, deadline), Ok(1000)); // count retained

        // Gating rewrites (still enabled + unmasked one-shot) must not re-arm.
        l.mmio_write(APIC_SVR, 0xFF | SVR_ENABLE_BIT, deadline)
            .unwrap();
        l.mmio_write(APIC_LVT_TIMER, 0x40, deadline).unwrap();
        assert_eq!(
            l.next_timer_deadline(),
            None,
            "a fired one-shot must not be resurrected by a gating write"
        );
        assert!(!l.advance_to(u64::MAX), "and must never fire again");
        assert!(!l.has_deliverable());

        // A fresh Initial Count write re-arms it.
        l.mmio_write(APIC_TMICT, 2000, deadline).unwrap();
        assert!(l.next_timer_deadline().is_some());
    }

    #[test]
    fn tdcr_change_midcount_reanchors_not_retroactive() {
        // PR #38 (6th timer bug): a Divide-Config write while the timer runs must
        // reschedule from the *current remaining count*, not apply the new
        // divisor retroactively (which gives a wrong deadline and can fire
        // immediately).
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_TDCR, 0b0000, 0).unwrap(); // ÷2
        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap(); // unmasked one-shot
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap(); // arm at t=0; period = 80_000 ns
        assert_eq!(l.next_timer_deadline(), Some(80_000));

        // Halfway: 500 ticks remain (1000 − floor(40_000·25e6/(2·1e9))).
        assert_eq!(l.mmio_read(APIC_TMCCT, 40_000), Ok(500));

        // Switch to ÷128 mid-count at t=40_000.
        l.mmio_write(APIC_TDCR, 0b1010, 40_000).unwrap();
        // The remaining count is preserved (NOT recomputed retroactively to 993).
        assert_eq!(l.mmio_read(APIC_TMCCT, 40_000), Ok(500));
        // Rescheduled from the remaining at the new rate: 40_000 + 500·128·40 ns.
        assert_eq!(l.next_timer_deadline(), Some(2_600_000));

        // Must not fire immediately or before the rescheduled deadline.
        assert!(!l.advance_to(40_000));
        assert!(!l.advance_to(2_599_999));
        assert!(l.advance_to(2_600_000));
        assert_eq!(l.take_interrupt(), Some(0x40));
    }

    #[test]
    fn eoi_via_mmio_retires_isr() {
        let mut l = enabled(25_000_000);
        l.raise(0x40).unwrap();
        assert_eq!(l.take_interrupt(), Some(0x40)); // 0x40 now in service
        // 0x40 -> ISR word 2 (64/32), bit 0.
        assert_eq!(l.mmio_read(APIC_ISR + 2 * 0x10, 0), Ok(1));
        // A write to the EOI register retires the highest in-service vector.
        l.mmio_write(APIC_EOI, 0, 0).unwrap();
        assert_eq!(l.mmio_read(APIC_ISR + 2 * 0x10, 0), Ok(0));
    }

    #[test]
    fn restore_reads_back_isr_tmr_irr_words() {
        // Distinct value per word constrains the read index arithmetic (a mutated
        // `(offset - BASE) >> 4` returns the wrong word), and a non-zero TMR —
        // which no normal operation produces — exercises its read arm.
        let template = Lapic::new(LapicConfig {
            apic_id: 0,
            timer_hz: 25_000_000,
        })
        .unwrap();
        let mut state = template.snapshot();
        let words = [0x1u32, 0x2, 0x4, 0x8, 0x10, 0x20, 0x40, 0x80];
        state.isr = words;
        state.tmr = words;
        state.irr = words;
        let l = Lapic::restore(&state).unwrap();
        for w in 0..8u32 {
            let i = w as usize;
            assert_eq!(
                l.mmio_read(APIC_ISR + w * 0x10, 0),
                Ok(words[i]),
                "isr w{w}"
            );
            assert_eq!(
                l.mmio_read(APIC_TMR + w * 0x10, 0),
                Ok(words[i]),
                "tmr w{w}"
            );
            assert_eq!(
                l.mmio_read(APIC_IRR + w * 0x10, 0),
                Ok(words[i]),
                "irr w{w}"
            );
        }
    }

    #[test]
    fn tmict_while_masked_arms_on_unmask() {
        // PR #38 re-review: a count loaded while the LVT timer is masked must arm
        // when the timer is unmasked.
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_LVT_TIMER, 0x40 | LVT_MASK_BIT, 0)
            .unwrap(); // masked one-shot
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert_eq!(l.next_timer_deadline(), None); // masked: not armed
        assert_eq!(l.mmio_read(APIC_TMICT, 0), Ok(1000)); // count retained
        // Unmask: the loaded count arms at the unmask instant.
        l.mmio_write(APIC_LVT_TIMER, 0x40, 100).unwrap();
        assert_eq!(l.next_timer_deadline(), Some(100 + 80_000)); // ÷2, N=1000 -> 80_000 ns
    }

    #[test]
    fn masking_running_timer_cancels_it() {
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap(); // unmasked one-shot
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap(); // arms
        assert!(l.next_timer_deadline().is_some());
        l.mmio_write(APIC_LVT_TIMER, 0x40 | LVT_MASK_BIT, 0)
            .unwrap(); // mask -> cancel
        assert_eq!(l.next_timer_deadline(), None);
        assert_eq!(l.mmio_read(APIC_TMCCT, 1000), Ok(0)); // not counting
    }

    #[test]
    fn enabling_apic_arms_loaded_timer() {
        // A count loaded while the APIC is software-disabled arms on enable.
        let mut l = Lapic::new(LapicConfig {
            apic_id: 0,
            timer_hz: 25_000_000,
        })
        .unwrap();
        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap(); // unmasked one-shot (still disabled)
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert_eq!(l.next_timer_deadline(), None); // disabled: not armed
        l.mmio_write(APIC_SVR, 0xFF | SVR_ENABLE_BIT, 50).unwrap(); // enable at t=50
        assert_eq!(l.next_timer_deadline(), Some(50 + 80_000));
    }

    #[test]
    fn changing_timer_mode_to_tsc_deadline_cancels() {
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap(); // one-shot, unmasked
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap(); // arms
        assert!(l.next_timer_deadline().is_some());
        // Mode 0b10 (TSC-deadline) is unsupported here -> cancel.
        l.mmio_write(APIC_LVT_TIMER, 0x40 | (0b10 << 17), 0)
            .unwrap();
        assert_eq!(l.next_timer_deadline(), None);
    }
}
