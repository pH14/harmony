// SPDX-License-Identifier: AGPL-3.0-or-later
//! The GICv3 model: register files, arbitration, and the virtual timer.

use crate::error::GicError;
use crate::state::{
    BITMAP_WORDS, CNTV_CTL_ENABLE, CNTV_CTL_IMASK, GIC_STATE_VERSION, GICD_FRAME_SIZE,
    GICR_FRAME_SIZE, GicState, PRIORITY_BYTES, SGI_PPI_COUNT,
};

/// Which MMIO frame an access targets. The composition root fixes the two
/// frames' guest-physical bases; the model sees only frame-relative offsets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GicFrame {
    /// The distributor frame (`GICD_*`, 64 KiB).
    Dist,
    /// The redistributor frame pair (RD frame at `0x0_0000`, SGI frame at
    /// `0x1_0000`; 128 KiB total — one redistributor, single vCPU).
    Redist,
}

/// Constructor input: the distributor bound, the timer frequency, and the
/// timer's PPI identity.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GicConfig {
    /// Implemented SPI count. A multiple of 32 in `0..=960` (so the top
    /// implemented INTID `32 + impl_spis - 1` never exceeds the architectural
    /// 1019 and `GICD_TYPER.ITLinesNumber` is exact).
    pub impl_spis: u32,
    /// The generic-timer counter frequency in Hz (the DTB `CNTFRQ` value; a
    /// documented composition constant, like x86's `LAPIC_TIMER_HZ` — never a
    /// measured quantity). Must be non-zero.
    pub timer_hz: u64,
    /// The virtual timer's PPI INTID (`16..32`; conventionally 27, the DT
    /// binding's virtual-timer interrupt).
    pub timer_intid: u32,
}

// --- distributor / redistributor register offsets -------------------------

const GICD_CTLR: u64 = 0x0000;
const GICD_TYPER: u64 = 0x0004;
const GICD_IIDR: u64 = 0x0008;
const IGROUPR_BASE: u64 = 0x0080;
const ISENABLER_BASE: u64 = 0x0100;
const ICENABLER_BASE: u64 = 0x0180;
const ISPENDR_BASE: u64 = 0x0200;
const ICPENDR_BASE: u64 = 0x0280;
const ISACTIVER_BASE: u64 = 0x0300;
const ICACTIVER_BASE: u64 = 0x0380;
const IPRIORITYR_BASE: u64 = 0x0400;
const IPRIORITYR_END: u64 = 0x0400 + PRIORITY_BYTES as u64; // one byte per INTID

/// `GICD_CTLR.EnableGrp1` (single security state, ARE=1): gates Group-1
/// forwarding — the one delivery group the model arbitrates.
const GICD_CTLR_ENABLE_GRP1: u32 = 1 << 1;
/// `GICD_CTLR.ARE` — affinity routing enable. The model *is* an ARE=1 GICv3;
/// the bit reads as one and writes to it are dropped.
const GICD_CTLR_ARE: u32 = 1 << 4;
/// The writable `GICD_CTLR` bits (the two group enables; everything else is
/// RO/RES0 here).
const GICD_CTLR_WRITE_MASK: u32 = 0b11;

/// `GICR_TYPER.Last` (bit 4) — this is the last (only) redistributor.
const GICR_TYPER_LAST: u32 = 1 << 4;
const GICR_CTLR: u64 = 0x0000;
const GICR_IIDR: u64 = 0x0004;
const GICR_TYPER_LO: u64 = 0x0008;
const GICR_TYPER_HI: u64 = 0x000C;
const GICR_WAKER: u64 = 0x0014;
/// The SGI frame starts one 64 KiB frame above the RD frame.
const SGI_FRAME_BASE: u64 = 0x1_0000;

/// "No running priority": the idle priority, strictly lower priority (higher
/// value) than any 8-bit interrupt priority.
const IDLE_PRIORITY: u16 = 256;

/// The deterministic userspace GICv3 (distributor + one redistributor + the
/// EL1 virtual timer), single vCPU. See the crate docs for the model's scope
/// and its skeleton limits.
#[derive(Clone, Debug)]
pub struct Gicv3 {
    impl_spis: u32,
    timer_hz: u64,
    timer_intid: u32,
    gicd_ctlr: u32,
    group: [u32; BITMAP_WORDS],
    enable: [u32; BITMAP_WORDS],
    pending: [u32; BITMAP_WORDS],
    active: [u32; BITMAP_WORDS],
    priority: [u8; PRIORITY_BYTES],
    pmr: u8,
    cntv_ctl: u64,
    cntv_cval: u64,
    timer_fired: bool,
}

impl Gicv3 {
    /// A reset GICv3 for `cfg`.
    ///
    /// # Errors
    /// [`GicError::InvalidState`] if `impl_spis` is not a multiple of 32 in
    /// `0..=960`, `timer_hz` is zero, or `timer_intid` is not a PPI.
    pub fn new(cfg: GicConfig) -> Result<Gicv3, GicError> {
        if !cfg.impl_spis.is_multiple_of(32) || cfg.impl_spis > 960 {
            return Err(GicError::InvalidState);
        }
        if cfg.timer_hz == 0 {
            return Err(GicError::InvalidState);
        }
        if !(16..SGI_PPI_COUNT).contains(&cfg.timer_intid) {
            return Err(GicError::InvalidState);
        }
        Ok(Gicv3 {
            impl_spis: cfg.impl_spis,
            timer_hz: cfg.timer_hz,
            timer_intid: cfg.timer_intid,
            gicd_ctlr: 0,
            group: [0; BITMAP_WORDS],
            enable: [0; BITMAP_WORDS],
            pending: [0; BITMAP_WORDS],
            active: [0; BITMAP_WORDS],
            priority: [0; PRIORITY_BYTES],
            pmr: 0,
            cntv_ctl: 0,
            cntv_cval: 0,
            timer_fired: false,
        })
    }

    /// One past the highest implemented INTID (`32 + impl_spis`).
    pub fn intid_limit(&self) -> u32 {
        SGI_PPI_COUNT + self.impl_spis
    }

    /// `true` iff `intid` lies inside the implemented identity space.
    pub fn implemented(&self, intid: u32) -> bool {
        intid < self.intid_limit()
    }

    // --- MMIO ----------------------------------------------------------------

    /// Service a 32-bit register load at `offset` inside `frame`, at V-time
    /// `now_vns` (unused by the modeled registers today; taken for seam parity
    /// with `lapic::mmio_read` so a future current-count-style register slots
    /// in without a signature change). Unmodeled in-range registers read `0`.
    ///
    /// # Errors
    /// [`GicError::BadOffset`] for an out-of-frame or unaligned offset.
    pub fn mmio_read(&self, frame: GicFrame, offset: u64, now_vns: u64) -> Result<u32, GicError> {
        let _ = now_vns;
        self.check_offset(frame, offset)?;
        Ok(match frame {
            GicFrame::Dist => self.dist_read(offset),
            GicFrame::Redist => self.redist_read(offset),
        })
    }

    /// Service a 32-bit register store. Read-only and unmodeled in-range
    /// registers deny-ignore (the write is dropped); set/clear banks apply
    /// masked to the implemented INTID range.
    ///
    /// # Errors
    /// [`GicError::BadOffset`] for an out-of-frame or unaligned offset.
    pub fn mmio_write(
        &mut self,
        frame: GicFrame,
        offset: u64,
        value: u32,
        now_vns: u64,
    ) -> Result<(), GicError> {
        let _ = now_vns;
        self.check_offset(frame, offset)?;
        match frame {
            GicFrame::Dist => self.dist_write(offset, value),
            GicFrame::Redist => self.redist_write(offset, value),
        }
        Ok(())
    }

    fn check_offset(&self, frame: GicFrame, offset: u64) -> Result<(), GicError> {
        let size = match frame {
            GicFrame::Dist => GICD_FRAME_SIZE,
            GicFrame::Redist => GICR_FRAME_SIZE,
        };
        if offset >= size || !offset.is_multiple_of(4) {
            return Err(GicError::BadOffset(offset));
        }
        Ok(())
    }

    /// The distributor's `TYPER` value: `ITLinesNumber` from the configured
    /// SPI bound, and a 10-bit INTID space (`IDbits = 9`).
    fn gicd_typer(&self) -> u32 {
        (self.intid_limit() / 32 - 1) | (9 << 19)
    }

    fn dist_read(&self, offset: u64) -> u32 {
        match offset {
            GICD_CTLR => self.gicd_ctlr | GICD_CTLR_ARE,
            GICD_TYPER => self.gicd_typer(),
            GICD_IIDR => 0,
            _ => {
                // The banked-per-INTID files: the distributor owns SPIs only
                // (word index ≥ 1 / priority byte ≥ 32); the SGI/PPI bank is
                // the redistributor's and reads RES0 here (ARE=1).
                if let Some(w) = word_index(offset, IGROUPR_BASE) {
                    return if w == 0 { 0 } else { self.group[w] };
                }
                if let Some(w) = word_index(offset, ISENABLER_BASE) {
                    return if w == 0 { 0 } else { self.enable[w] };
                }
                if let Some(w) = word_index(offset, ICENABLER_BASE) {
                    return if w == 0 { 0 } else { self.enable[w] };
                }
                if let Some(w) = word_index(offset, ISPENDR_BASE) {
                    return if w == 0 { 0 } else { self.pending[w] };
                }
                if let Some(w) = word_index(offset, ICPENDR_BASE) {
                    return if w == 0 { 0 } else { self.pending[w] };
                }
                if let Some(w) = word_index(offset, ISACTIVER_BASE) {
                    return if w == 0 { 0 } else { self.active[w] };
                }
                if let Some(w) = word_index(offset, ICACTIVER_BASE) {
                    return if w == 0 { 0 } else { self.active[w] };
                }
                if (IPRIORITYR_BASE..IPRIORITYR_END).contains(&offset) {
                    let first = (offset - IPRIORITYR_BASE) as usize;
                    if first < SGI_PPI_COUNT as usize {
                        return 0; // SGI/PPI priorities are redistributor-banked
                    }
                    return self.priority_word(first);
                }
                0
            }
        }
    }

    fn dist_write(&mut self, offset: u64, value: u32) {
        match offset {
            GICD_CTLR => self.gicd_ctlr = value & GICD_CTLR_WRITE_MASK,
            _ => {
                // Distributor banks own SPIs only; word 0 / bytes 0..32 are
                // the redistributor's and deny-ignore here.
                if let Some(w) = word_index(offset, IGROUPR_BASE) {
                    if w != 0 {
                        self.group[w] = value & self.word_mask(w);
                    }
                    return;
                }
                if let Some(w) = word_index(offset, ISENABLER_BASE) {
                    if w != 0 {
                        self.enable[w] |= value & self.word_mask(w);
                    }
                    return;
                }
                if let Some(w) = word_index(offset, ICENABLER_BASE) {
                    if w != 0 {
                        self.enable[w] &= !value;
                    }
                    return;
                }
                if let Some(w) = word_index(offset, ISPENDR_BASE) {
                    if w != 0 {
                        self.pending[w] |= value & self.word_mask(w);
                    }
                    return;
                }
                if let Some(w) = word_index(offset, ICPENDR_BASE) {
                    if w != 0 {
                        self.pending[w] &= !value;
                    }
                    return;
                }
                if let Some(w) = word_index(offset, ISACTIVER_BASE) {
                    if w != 0 {
                        self.active[w] |= value & self.word_mask(w);
                    }
                    return;
                }
                if let Some(w) = word_index(offset, ICACTIVER_BASE) {
                    if w != 0 {
                        self.active[w] &= !value;
                    }
                    return;
                }
                if (IPRIORITYR_BASE..IPRIORITYR_END).contains(&offset) {
                    let first = (offset - IPRIORITYR_BASE) as usize;
                    if first >= SGI_PPI_COUNT as usize {
                        self.write_priority_word(first, value);
                    }
                }
            }
        }
    }

    fn redist_read(&self, offset: u64) -> u32 {
        if offset < SGI_FRAME_BASE {
            // The RD frame.
            return match offset {
                GICR_CTLR | GICR_IIDR => 0,
                GICR_TYPER_LO => GICR_TYPER_LAST,
                GICR_TYPER_HI => 0,
                GICR_WAKER => 0, // awake: ProcessorSleep=0, ChildrenAsleep=0
                _ => 0,
            };
        }
        // The SGI frame: the banked SGI/PPI files (word 0 / bytes 0..32).
        let r = offset - SGI_FRAME_BASE;
        match r {
            _ if word_index(r, IGROUPR_BASE) == Some(0) => self.group[0],
            _ if word_index(r, ISENABLER_BASE) == Some(0)
                || word_index(r, ICENABLER_BASE) == Some(0) =>
            {
                self.enable[0]
            }
            _ if word_index(r, ISPENDR_BASE) == Some(0)
                || word_index(r, ICPENDR_BASE) == Some(0) =>
            {
                self.pending[0]
            }
            _ if word_index(r, ISACTIVER_BASE) == Some(0)
                || word_index(r, ICACTIVER_BASE) == Some(0) =>
            {
                self.active[0]
            }
            _ if (IPRIORITYR_BASE..IPRIORITYR_BASE + u64::from(SGI_PPI_COUNT)).contains(&r) => {
                self.priority_word((r - IPRIORITYR_BASE) as usize)
            }
            _ => 0,
        }
    }

    fn redist_write(&mut self, offset: u64, value: u32) {
        if offset < SGI_FRAME_BASE {
            return; // RD-frame registers: RO or unmodeled — deny-ignore.
        }
        let r = offset - SGI_FRAME_BASE;
        if word_index(r, IGROUPR_BASE) == Some(0) {
            self.group[0] = value;
        } else if word_index(r, ISENABLER_BASE) == Some(0) {
            self.enable[0] |= value;
        } else if word_index(r, ICENABLER_BASE) == Some(0) {
            self.enable[0] &= !value;
        } else if word_index(r, ISPENDR_BASE) == Some(0) {
            self.pending[0] |= value;
        } else if word_index(r, ICPENDR_BASE) == Some(0) {
            self.pending[0] &= !value;
        } else if word_index(r, ISACTIVER_BASE) == Some(0) {
            self.active[0] |= value;
        } else if word_index(r, ICACTIVER_BASE) == Some(0) {
            self.active[0] &= !value;
        } else if (IPRIORITYR_BASE..IPRIORITYR_BASE + u64::from(SGI_PPI_COUNT)).contains(&r) {
            self.write_priority_word((r - IPRIORITYR_BASE) as usize, value);
        }
    }

    /// The bits of bitmap word `w` that address implemented INTIDs.
    fn word_mask(&self, w: usize) -> u32 {
        let limit = self.intid_limit() as usize;
        let base = w * 32;
        if base + 32 <= limit {
            u32::MAX
        } else if base >= limit {
            0
        } else {
            // Partial words cannot occur (the limit is a multiple of 32), but
            // stay total rather than trusting the invariant.
            u32::MAX >> (32 - (limit - base))
        }
    }

    fn priority_word(&self, first_byte: usize) -> u32 {
        let mut v = 0u32;
        for i in 0..4 {
            let idx = first_byte + i;
            let b = if idx < PRIORITY_BYTES {
                self.priority[idx]
            } else {
                0
            };
            v |= u32::from(b) << (8 * i);
        }
        v
    }

    fn write_priority_word(&mut self, first_byte: usize, value: u32) {
        let limit = self.intid_limit() as usize;
        for i in 0..4 {
            let idx = first_byte + i;
            if idx < limit && idx < PRIORITY_BYTES {
                self.priority[idx] = (value >> (8 * i)) as u8;
            }
        }
    }

    // --- interrupt file ------------------------------------------------------

    /// Raise `intid` pending (the host-injection / device-line entry point;
    /// normal arbitration then delivers it).
    ///
    /// # Errors
    /// [`GicError::BadIntId`] outside the implemented identity space.
    pub fn raise(&mut self, intid: u32) -> Result<(), GicError> {
        if !self.implemented(intid) {
            return Err(GicError::BadIntId(intid));
        }
        self.pending[(intid / 32) as usize] |= 1 << (intid % 32);
        Ok(())
    }

    /// The running priority: the highest priority (lowest value) among active
    /// interrupts, or the idle priority when none is active.
    fn running_priority(&self) -> u16 {
        let mut best = IDLE_PRIORITY;
        for w in 0..BITMAP_WORDS {
            let mut bits = self.active[w];
            while bits != 0 {
                let bit = bits.trailing_zeros();
                bits &= bits - 1;
                let intid = (w as u32) * 32 + bit;
                if self.implemented(intid) {
                    best = best.min(u16::from(self.priority[intid as usize]));
                }
            }
        }
        best
    }

    /// The one highest-priority **deliverable** Group-1 INTID, without any
    /// state transition: pending ∧ enabled ∧ Group 1 ∧ `GICD_CTLR.EnableGrp1`
    /// ∧ priority strictly higher (value strictly lower) than both `PMR` and
    /// the running priority. Ties resolve to the lowest INTID (deterministic).
    pub fn peek_interrupt(&self) -> Option<u32> {
        if self.gicd_ctlr & GICD_CTLR_ENABLE_GRP1 == 0 {
            return None;
        }
        let running = self.running_priority();
        let pmr = u16::from(self.pmr);
        let mut best: Option<(u16, u32)> = None;
        for w in 0..BITMAP_WORDS {
            let mut bits = self.pending[w] & self.enable[w] & self.group[w] & !self.active[w];
            while bits != 0 {
                let bit = bits.trailing_zeros();
                bits &= bits - 1;
                let intid = (w as u32) * 32 + bit;
                if !self.implemented(intid) {
                    continue;
                }
                let prio = u16::from(self.priority[intid as usize]);
                if prio >= pmr || prio >= running {
                    continue;
                }
                let key = (prio, intid);
                if best.is_none_or(|b| key < b) {
                    best = Some(key);
                }
            }
        }
        best.map(|(_, intid)| intid)
    }

    /// `true` iff [`Gicv3::peek_interrupt`] would return an INTID.
    pub fn has_deliverable(&self) -> bool {
        self.peek_interrupt().is_some()
    }

    /// Acknowledge the arbitrated INTID: the pending→active transition (the
    /// `ICC_IAR1_EL1` read on real hardware). vmm-core calls this only once
    /// the backend confirms acceptance, so a snapshot taken while the INTID
    /// awaits injection shows it pending, not prematurely in service.
    pub fn take_interrupt(&mut self) -> Option<u32> {
        let intid = self.peek_interrupt()?;
        let (w, b) = ((intid / 32) as usize, intid % 32);
        self.pending[w] &= !(1 << b);
        self.active[w] |= 1 << b;
        Some(intid)
    }

    /// End of interrupt for `intid`: clear its active bit (the combined
    /// priority-drop + deactivate of `ICC_EOIR1_EL1`/`ICC_DIR_EL1` with
    /// `EOImode == 0`).
    ///
    /// # Errors
    /// [`GicError::BadIntId`] outside the implemented identity space.
    pub fn eoi(&mut self, intid: u32) -> Result<(), GicError> {
        if !self.implemented(intid) {
            return Err(GicError::BadIntId(intid));
        }
        self.active[(intid / 32) as usize] &= !(1 << (intid % 32));
        Ok(())
    }

    /// Set the CPU interface's priority mask (`ICC_PMR_EL1`; a sysreg on real
    /// hardware — `TODO(patched-abi)` for the trap surface).
    pub fn set_pmr(&mut self, pmr: u8) {
        self.pmr = pmr;
    }

    /// The current priority mask.
    pub fn pmr(&self) -> u8 {
        self.pmr
    }

    // --- the virtual timer ----------------------------------------------------

    /// Write `CNTV_CTL_EL0` (`ENABLE` | `IMASK`; other bits drop). Re-arms the
    /// one-shot pending latch — the fired bookkeeping belongs to an arming,
    /// and reprogramming the control starts a new one.
    pub fn write_cntv_ctl(&mut self, value: u64) {
        self.cntv_ctl = value & (CNTV_CTL_ENABLE | CNTV_CTL_IMASK);
        self.timer_fired = false;
    }

    /// Write `CNTV_CVAL_EL0` (the absolute compare value in timer ticks).
    /// Re-arms the one-shot pending latch.
    pub fn write_cntv_cval(&mut self, value: u64) {
        self.cntv_cval = value;
        self.timer_fired = false;
    }

    /// Read `CNTV_CTL_EL0`, with `ISTATUS` (bit 2) reflecting whether the
    /// timer condition holds at `now_vns`.
    pub fn read_cntv_ctl(&self, now_vns: u64) -> u64 {
        let mut v = self.cntv_ctl;
        if self.cntv_ctl & CNTV_CTL_ENABLE != 0 && self.deadline_vns().is_some_and(|d| now_vns >= d)
        {
            v |= 1 << 2;
        }
        v
    }

    /// Read `CNTV_CVAL_EL0`.
    pub fn read_cntv_cval(&self) -> u64 {
        self.cntv_cval
    }

    /// The armed deadline in V-time ns: `Some` iff the timer is enabled,
    /// unmasked, and has not yet latched its edge — and the tick→ns conversion
    /// is representable (an unrepresentable deadline is no deadline, never a
    /// clamped `u64::MAX` that would fire spuriously; `lapic`'s discipline).
    pub fn next_timer_deadline(&self) -> Option<u64> {
        if self.cntv_ctl & CNTV_CTL_ENABLE == 0
            || self.cntv_ctl & CNTV_CTL_IMASK != 0
            || self.timer_fired
        {
            return None;
        }
        self.deadline_vns()
    }

    /// `CVAL` ticks → whole V-time ns, exact integer ceiling (`u128`): the
    /// deadline is the first nanosecond at which the counter — which advances
    /// at `timer_hz` on the V-time axis (the clock page's mapping, `hm-rk5`)
    /// — reaches the compare value.
    fn deadline_vns(&self) -> Option<u64> {
        let ns = (u128::from(self.cntv_cval) * 1_000_000_000).div_ceil(u128::from(self.timer_hz));
        u64::try_from(ns).ok()
    }

    /// Whether the armed timer's fire would actually deliver: its PPI is
    /// enabled, Group 1, group-forwarded, and passes the PMR against the
    /// current running priority. An armed-but-undeliverable timer is no wake
    /// (the idle path's discriminator, exactly as `lapic`'s
    /// `armed_timer_deliverable`).
    pub fn armed_timer_deliverable(&self) -> bool {
        if self.next_timer_deadline().is_none() {
            return false;
        }
        let intid = self.timer_intid;
        let (w, b) = ((intid / 32) as usize, intid % 32);
        let wired = self.gicd_ctlr & GICD_CTLR_ENABLE_GRP1 != 0
            && self.enable[w] & (1 << b) != 0
            && self.group[w] & (1 << b) != 0;
        let prio = u16::from(self.priority[intid as usize]);
        wired && prio < u16::from(self.pmr) && prio < self.running_priority()
    }

    /// Advance the fabric to `now_vns`: latch the virtual timer's PPI pending
    /// when its deadline has passed. Returns `true` iff state changed.
    /// Idempotent for a given `now_vns`.
    pub fn advance_to(&mut self, now_vns: u64) -> bool {
        match self.next_timer_deadline() {
            Some(d) if now_vns >= d => {
                self.timer_fired = true;
                let (w, b) = ((self.timer_intid / 32) as usize, self.timer_intid % 32);
                self.pending[w] |= 1 << b;
                true
            }
            _ => false,
        }
    }

    // --- snapshot / restore ----------------------------------------------------

    /// Capture the full model state (plain data; deadlines derived, never
    /// stored).
    pub fn snapshot(&self) -> GicState {
        GicState {
            version: GIC_STATE_VERSION,
            impl_spis: self.impl_spis,
            timer_hz: self.timer_hz,
            timer_intid: self.timer_intid,
            gicd_ctlr: self.gicd_ctlr,
            group: self.group,
            enable: self.enable,
            pending: self.pending,
            active: self.active,
            priority: self.priority,
            pmr: self.pmr,
            cntv_ctl: self.cntv_ctl,
            cntv_cval: self.cntv_cval,
            timer_fired: self.timer_fired,
        }
    }

    /// Rebuild a model from a snapshot — a strict validation boundary
    /// (untrusted input): the version, the config invariants, the `CTLR` and
    /// `CNTV_CTL` write masks, and every register-file bit/byte beyond the
    /// implemented INTID range must be zero.
    ///
    /// # Errors
    /// [`GicError::InvalidState`] on any violated invariant.
    pub fn restore(state: &GicState) -> Result<Gicv3, GicError> {
        if state.version != GIC_STATE_VERSION {
            return Err(GicError::InvalidState);
        }
        let mut g = Gicv3::new(GicConfig {
            impl_spis: state.impl_spis,
            timer_hz: state.timer_hz,
            timer_intid: state.timer_intid,
        })?;
        if state.gicd_ctlr & !GICD_CTLR_WRITE_MASK != 0 {
            return Err(GicError::InvalidState);
        }
        if state.cntv_ctl & !(CNTV_CTL_ENABLE | CNTV_CTL_IMASK) != 0 {
            return Err(GicError::InvalidState);
        }
        for w in 0..BITMAP_WORDS {
            let mask = g.word_mask(w);
            for file in [&state.group, &state.enable, &state.pending, &state.active] {
                if file[w] & !mask != 0 {
                    return Err(GicError::InvalidState);
                }
            }
        }
        let limit = g.intid_limit() as usize;
        if state.priority[limit..].iter().any(|&b| b != 0) {
            return Err(GicError::InvalidState);
        }
        g.gicd_ctlr = state.gicd_ctlr;
        g.group = state.group;
        g.enable = state.enable;
        g.pending = state.pending;
        g.active = state.active;
        g.priority = state.priority;
        g.pmr = state.pmr;
        g.cntv_ctl = state.cntv_ctl;
        g.cntv_cval = state.cntv_cval;
        g.timer_fired = state.timer_fired;
        Ok(g)
    }
}

/// The bitmap word index a byte `offset` addresses inside a 128-byte
/// one-bit-per-INTID bank at `base`, or `None` if outside the bank.
fn word_index(offset: u64, base: u64) -> Option<usize> {
    if (base..base + 4 * BITMAP_WORDS as u64).contains(&offset) {
        Some(((offset - base) / 4) as usize)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gic() -> Gicv3 {
        Gicv3::new(GicConfig {
            impl_spis: 64,
            timer_hz: 62_500_000, // a typical CNTFRQ; any non-zero works
            timer_intid: 27,
        })
        .unwrap()
    }

    /// Program `intid` fully deliverable: Group 1, enabled, priority `prio`,
    /// group forwarding on, PMR open.
    fn arm(g: &mut Gicv3, intid: u32, prio: u8) {
        g.mmio_write(GicFrame::Dist, GICD_CTLR, GICD_CTLR_ENABLE_GRP1, 0)
            .unwrap();
        g.set_pmr(0xFF);
        let (w, b) = (intid / 32, intid % 32);
        // A 32-bit IPRIORITYR store writes all four priority bytes, so the
        // helper read-modify-writes to keep same-word neighbors intact.
        let shift = 8 * (intid % 4);
        if w == 0 {
            let sgi = SGI_FRAME_BASE;
            let grp = g
                .mmio_read(GicFrame::Redist, sgi + IGROUPR_BASE, 0)
                .unwrap();
            g.mmio_write(GicFrame::Redist, sgi + IGROUPR_BASE, grp | (1 << b), 0)
                .unwrap();
            g.mmio_write(GicFrame::Redist, sgi + ISENABLER_BASE, 1 << b, 0)
                .unwrap();
            let pr = sgi + IPRIORITYR_BASE + u64::from(intid & !3);
            let old = g.mmio_read(GicFrame::Redist, pr, 0).unwrap();
            let word = (old & !(0xFF << shift)) | (u32::from(prio) << shift);
            g.mmio_write(GicFrame::Redist, pr, word, 0).unwrap();
        } else {
            let goff = IGROUPR_BASE + u64::from(w) * 4;
            let grp = g.mmio_read(GicFrame::Dist, goff, 0).unwrap();
            g.mmio_write(GicFrame::Dist, goff, grp | (1 << b), 0)
                .unwrap();
            g.mmio_write(GicFrame::Dist, ISENABLER_BASE + u64::from(w) * 4, 1 << b, 0)
                .unwrap();
            let pr = IPRIORITYR_BASE + u64::from(intid & !3);
            let old = g.mmio_read(GicFrame::Dist, pr, 0).unwrap();
            let word = (old & !(0xFF << shift)) | (u32::from(prio) << shift);
            g.mmio_write(GicFrame::Dist, pr, word, 0).unwrap();
        }
    }

    #[test]
    fn reset_state_delivers_nothing() {
        let mut g = gic();
        g.raise(40).unwrap();
        // Group 0, disabled, PMR 0, forwarding off: nothing deliverable.
        assert_eq!(g.peek_interrupt(), None);
    }

    #[test]
    fn sgi_zero_delivers_when_programmed() {
        // The x86 `< 16 reserved` rule must never leak in: SGI 0 delivers.
        let mut g = gic();
        arm(&mut g, 0, 0x40);
        g.raise(0).unwrap();
        assert_eq!(g.peek_interrupt(), Some(0));
    }

    #[test]
    fn arbitration_picks_highest_priority_then_lowest_intid() {
        let mut g = gic();
        arm(&mut g, 40, 0x80);
        arm(&mut g, 41, 0x40); // higher priority (lower value)
        arm(&mut g, 42, 0x40); // tie with 41 → lowest INTID wins
        g.raise(40).unwrap();
        g.raise(42).unwrap();
        g.raise(41).unwrap();
        assert_eq!(g.peek_interrupt(), Some(41));
        assert_eq!(g.take_interrupt(), Some(41));
        // 41 active at 0x40: 42 (same priority) cannot preempt; 40 neither.
        assert_eq!(g.peek_interrupt(), None);
        g.eoi(41).unwrap();
        assert_eq!(g.take_interrupt(), Some(42));
        g.eoi(42).unwrap();
        assert_eq!(g.take_interrupt(), Some(40));
    }

    #[test]
    fn pmr_masks_delivery() {
        let mut g = gic();
        arm(&mut g, 40, 0x80);
        g.raise(40).unwrap();
        g.set_pmr(0x80); // equal priority is NOT strictly higher: masked
        assert_eq!(g.peek_interrupt(), None);
        g.set_pmr(0x81);
        assert_eq!(g.peek_interrupt(), Some(40));
    }

    #[test]
    fn unimplemented_intids_are_rejected_and_writes_masked() {
        let mut g = gic(); // limit = 96
        assert_eq!(g.raise(96), Err(GicError::BadIntId(96)));
        assert_eq!(g.raise(1023), Err(GicError::BadIntId(1023)));
        assert!(g.raise(95).is_ok());
        // A set-enable write to a word past the limit is dropped.
        g.mmio_write(GicFrame::Dist, ISENABLER_BASE + 3 * 4, u32::MAX, 0)
            .unwrap();
        assert_eq!(
            g.mmio_read(GicFrame::Dist, ISENABLER_BASE + 3 * 4, 0)
                .unwrap(),
            0
        );
    }

    #[test]
    fn timer_latches_pending_and_reports_deadline() {
        let mut g = gic();
        arm(&mut g, 27, 0x20);
        // CVAL = 125 ticks at 62.5 MHz ⇒ exactly 2000 ns.
        g.write_cntv_cval(125);
        g.write_cntv_ctl(CNTV_CTL_ENABLE);
        assert_eq!(g.next_timer_deadline(), Some(2000));
        assert!(g.armed_timer_deliverable());
        assert!(!g.advance_to(1999));
        assert!(g.advance_to(2000));
        assert_eq!(g.peek_interrupt(), Some(27));
        // The edge latched once; the deadline is consumed until re-armed.
        assert_eq!(g.next_timer_deadline(), None);
        assert!(!g.advance_to(3000));
        g.write_cntv_cval(250); // re-arm
        assert_eq!(g.next_timer_deadline(), Some(4000));
    }

    #[test]
    fn masked_or_disabled_timer_has_no_deadline() {
        let mut g = gic();
        g.write_cntv_cval(125);
        g.write_cntv_ctl(CNTV_CTL_ENABLE | CNTV_CTL_IMASK);
        assert_eq!(g.next_timer_deadline(), None);
        g.write_cntv_ctl(0);
        assert_eq!(g.next_timer_deadline(), None);
    }

    #[test]
    fn cntv_ctl_reads_back_istatus() {
        let mut g = gic();
        g.write_cntv_cval(125);
        g.write_cntv_ctl(CNTV_CTL_ENABLE);
        assert_eq!(g.read_cntv_ctl(0), CNTV_CTL_ENABLE);
        assert_eq!(g.read_cntv_ctl(2000), CNTV_CTL_ENABLE | (1 << 2));
        assert_eq!(g.read_cntv_cval(), 125);
    }

    #[test]
    fn snapshot_restores_bit_identically() {
        let mut g = gic();
        arm(&mut g, 40, 0x30);
        g.raise(40).unwrap();
        g.write_cntv_cval(125);
        g.write_cntv_ctl(CNTV_CTL_ENABLE);
        g.advance_to(5000);
        let s = g.snapshot();
        let r = Gicv3::restore(&s).unwrap();
        assert_eq!(r.snapshot(), s);
        assert_eq!(r.peek_interrupt(), g.peek_interrupt());
    }

    #[test]
    fn restore_rejects_state_past_the_implemented_range() {
        let g = gic();
        let mut s = g.snapshot();
        s.pending[4] = 1; // INTID 128 ≥ limit 96
        assert_eq!(Gicv3::restore(&s).unwrap_err(), GicError::InvalidState);
        let mut s = g.snapshot();
        s.priority[96] = 1;
        assert_eq!(Gicv3::restore(&s).unwrap_err(), GicError::InvalidState);
        let mut s = g.snapshot();
        s.version = 99;
        assert_eq!(Gicv3::restore(&s).unwrap_err(), GicError::InvalidState);
    }

    #[test]
    fn bad_offsets_error_and_unmodeled_offsets_deny_ignore() {
        let mut g = gic();
        assert_eq!(
            g.mmio_read(GicFrame::Dist, GICD_FRAME_SIZE, 0),
            Err(GicError::BadOffset(GICD_FRAME_SIZE))
        );
        assert_eq!(
            g.mmio_read(GicFrame::Dist, 2, 0),
            Err(GicError::BadOffset(2))
        );
        // Unmodeled in-range: read 0, write dropped.
        assert_eq!(g.mmio_read(GicFrame::Dist, 0x0C00, 0).unwrap(), 0);
        g.mmio_write(GicFrame::Dist, 0x0C00, 0xFFFF_FFFF, 0)
            .unwrap();
        assert_eq!(g.mmio_read(GicFrame::Dist, 0x0C00, 0).unwrap(), 0);
        // TYPER encodes the configured limit: (32+64)/32 - 1 = 2.
        let typer = g.mmio_read(GicFrame::Dist, GICD_TYPER, 0).unwrap();
        assert_eq!(typer & 0x1F, 2);
    }

    #[test]
    fn config_invariants_are_enforced() {
        let bad = |c: GicConfig| Gicv3::new(c).unwrap_err();
        assert_eq!(
            bad(GicConfig {
                impl_spis: 33,
                timer_hz: 1,
                timer_intid: 27
            }),
            GicError::InvalidState
        );
        assert_eq!(
            bad(GicConfig {
                impl_spis: 992,
                timer_hz: 1,
                timer_intid: 27
            }),
            GicError::InvalidState
        );
        assert_eq!(
            bad(GicConfig {
                impl_spis: 64,
                timer_hz: 0,
                timer_intid: 27
            }),
            GicError::InvalidState
        );
        assert_eq!(
            bad(GicConfig {
                impl_spis: 64,
                timer_hz: 1,
                timer_intid: 32
            }),
            GicError::InvalidState
        );
        assert_eq!(
            bad(GicConfig {
                impl_spis: 64,
                timer_hz: 1,
                timer_intid: 15
            }),
            GicError::InvalidState
        );
    }
}
