// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::error::GicError;
use crate::state::{
    BITMAP_WORDS, CNTV_CTL_ENABLE, CNTV_CTL_IMASK, GIC_STATE_VERSION, GICD_FRAME_SIZE,
    GICR_FRAME_SIZE, GicState, PRIORITY_BYTES, SGI_PPI_COUNT,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GicFrame {
    Dist,
    Redist,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct GicConfig {
    pub impl_spis: u32,
    pub timer_hz: u64,
    pub timer_intid: u32,
}

const GICD_CTLR: u64 = 0x0000;
const GICD_TYPER: u64 = 0x0004;
const GICD_IIDR: u64 = 0x0008;
const GICD_PIDR2: u64 = 0xFFE8;
const IGROUPR_BASE: u64 = 0x0080;
const ISENABLER_BASE: u64 = 0x0100;
const ICENABLER_BASE: u64 = 0x0180;
const ISPENDR_BASE: u64 = 0x0200;
const ICPENDR_BASE: u64 = 0x0280;
const ISACTIVER_BASE: u64 = 0x0300;
const ICACTIVER_BASE: u64 = 0x0380;
const IPRIORITYR_BASE: u64 = 0x0400;
const IPRIORITYR_END: u64 = 0x0400 + PRIORITY_BYTES as u64;

const GICD_CTLR_ENABLE_GRP1: u32 = 1 << 1;
const GICD_CTLR_ARE: u32 = 1 << 4;
const GICD_CTLR_DS: u32 = 1 << 6;
const GICD_CTLR_WRITE_MASK: u32 = GICD_CTLR_ENABLE_GRP1;

const GICR_TYPER_LAST: u32 = 1 << 4;
const GICR_CTLR_IR: u32 = 1 << 2;
const GICR_CTLR: u64 = 0x0000;
const GICR_IIDR: u64 = 0x0004;
const GICR_TYPER_LO: u64 = 0x0008;
const GICR_TYPER_HI: u64 = 0x000C;
const GICR_WAKER: u64 = 0x0014;
const GICR_PIDR2: u64 = 0xFFE8;
const GIC_PIDR2_ARCH_GICV3: u32 = 3 << 4;
const SGI_FRAME_BASE: u64 = 0x1_0000;

const IDLE_PRIORITY: u16 = 256;

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
    line_level: [u32; BITMAP_WORDS],
    priority: [u8; PRIORITY_BYTES],
    pmr: u8,
    igrpen1: bool,
    cntv_ctl: u64,
    cntv_cval: u64,
    timer_fired: bool,
}

impl Gicv3 {
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
        let mut group = [0; BITMAP_WORDS];
        for word in group
            .iter_mut()
            .take((SGI_PPI_COUNT + cfg.impl_spis) as usize / 32)
        {
            *word = u32::MAX;
        }
        let mut enable = [0; BITMAP_WORDS];
        enable[0] = u32::from(u16::MAX);
        Ok(Gicv3 {
            impl_spis: cfg.impl_spis,
            timer_hz: cfg.timer_hz,
            timer_intid: cfg.timer_intid,
            gicd_ctlr: 0,
            group,
            enable,
            pending: [0; BITMAP_WORDS],
            active: [0; BITMAP_WORDS],
            line_level: [0; BITMAP_WORDS],
            priority: [0; PRIORITY_BYTES],
            pmr: 0,
            igrpen1: false,
            cntv_ctl: 0,
            cntv_cval: 0,
            timer_fired: false,
        })
    }

    pub fn config(&self) -> GicConfig {
        GicConfig {
            impl_spis: self.impl_spis,
            timer_hz: self.timer_hz,
            timer_intid: self.timer_intid,
        }
    }

    pub fn intid_limit(&self) -> u32 {
        SGI_PPI_COUNT + self.impl_spis
    }

    pub fn implemented(&self, intid: u32) -> bool {
        intid < self.intid_limit()
    }

    pub fn mmio_read(&self, frame: GicFrame, offset: u64, now_vns: u64) -> Result<u32, GicError> {
        let _ = now_vns;
        self.check_offset(frame, offset)?;
        Ok(match frame {
            GicFrame::Dist => self.dist_read(offset),
            GicFrame::Redist => self.redist_read(offset),
        })
    }

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

    fn gicd_typer(&self) -> u32 {
        (self.intid_limit() / 32 - 1) | (9 << 19)
    }

    fn dist_read(&self, offset: u64) -> u32 {
        match offset {
            GICD_CTLR => self.gicd_ctlr | GICD_CTLR_ARE | GICD_CTLR_DS,
            GICD_TYPER => self.gicd_typer(),
            GICD_IIDR => 0,
            GICD_PIDR2 => GIC_PIDR2_ARCH_GICV3,
            _ => {
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
                        return 0;
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
            return match offset {
                GICR_CTLR => GICR_CTLR_IR,
                GICR_IIDR => 0,
                GICR_TYPER_LO => GICR_TYPER_LAST,
                GICR_TYPER_HI => 0,
                GICR_WAKER => 0,
                GICR_PIDR2 => GIC_PIDR2_ARCH_GICV3,
                _ => 0,
            };
        }
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
            return;
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

    fn word_mask(&self, w: usize) -> u32 {
        let limit = self.intid_limit() as usize;
        let base = w * 32;
        if base + 32 <= limit {
            u32::MAX
        } else if base >= limit {
            0
        } else {
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

    pub fn raise(&mut self, intid: u32) -> Result<(), GicError> {
        if !self.implemented(intid) {
            return Err(GicError::BadIntId(intid));
        }
        self.pending[(intid / 32) as usize] |= 1 << (intid % 32);
        Ok(())
    }

    pub fn lower(&mut self, intid: u32) -> Result<(), GicError> {
        if !self.implemented(intid) {
            return Err(GicError::BadIntId(intid));
        }
        self.pending[(intid / 32) as usize] &= !(1 << (intid % 32));
        Ok(())
    }

    pub fn pulse(&mut self, intid: u32) -> Result<(), GicError> {
        self.raise(intid)
    }

    pub fn assert_line(&mut self, intid: u32) -> Result<(), GicError> {
        if !self.implemented(intid) {
            return Err(GicError::BadIntId(intid));
        }
        self.line_level[(intid / 32) as usize] |= 1 << (intid % 32);
        Ok(())
    }

    pub fn deassert_line(&mut self, intid: u32) -> Result<(), GicError> {
        if !self.implemented(intid) {
            return Err(GicError::BadIntId(intid));
        }
        self.line_level[(intid / 32) as usize] &= !(1 << (intid % 32));
        Ok(())
    }

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

    pub fn peek_interrupt(&self) -> Option<u32> {
        if self.gicd_ctlr & GICD_CTLR_ENABLE_GRP1 == 0 || !self.igrpen1 {
            return None;
        }
        let running = self.running_priority();
        let pmr = u16::from(self.pmr);
        let mut best: Option<(u16, u32)> = None;
        for w in 0..BITMAP_WORDS {
            let mut bits = (self.pending[w] | self.line_level[w])
                & self.enable[w]
                & self.group[w]
                & !self.active[w];
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

    pub fn has_deliverable(&self) -> bool {
        self.peek_interrupt().is_some()
    }

    pub fn input_deliverable(&self, intid: u32) -> bool {
        if !self.implemented(intid) || self.gicd_ctlr & GICD_CTLR_ENABLE_GRP1 == 0 || !self.igrpen1
        {
            return false;
        }
        let (w, b) = ((intid / 32) as usize, intid % 32);
        if self.enable[w] & (1 << b) == 0
            || self.group[w] & (1 << b) == 0
            || bitmap_contains(&self.active, intid)
        {
            return false;
        }
        let priority = u16::from(self.priority[intid as usize]);
        priority < u16::from(self.pmr) && priority < self.running_priority()
    }

    pub fn take_interrupt(&mut self) -> Option<u32> {
        let intid = self.peek_interrupt()?;
        let (w, b) = ((intid / 32) as usize, intid % 32);
        self.pending[w] &= !(1 << b);
        self.active[w] |= 1 << b;
        Some(intid)
    }

    pub fn active_interrupt(&self) -> Option<u32> {
        let mut best: Option<(u16, u32)> = None;
        for word in 0..BITMAP_WORDS {
            for bit in 0..32 {
                if self.active[word] & (1 << bit) == 0 {
                    continue;
                }
                let intid = word as u32 * 32 + bit;
                if !self.implemented(intid) {
                    continue;
                }
                let key = (u16::from(self.priority[intid as usize]), intid);
                if best.is_none_or(|current| key.cmp(&current).is_lt()) {
                    best = Some(key);
                }
            }
        }
        best.map(|(_, intid)| intid)
    }

    pub fn eoi(&mut self, intid: u32) -> Result<(), GicError> {
        if !self.implemented(intid) {
            return Err(GicError::BadIntId(intid));
        }
        self.active[(intid / 32) as usize] &= !(1 << (intid % 32));
        Ok(())
    }

    pub fn set_pmr(&mut self, pmr: u8) {
        self.pmr = pmr;
    }

    pub fn pmr(&self) -> u8 {
        self.pmr
    }

    pub fn set_group1_enabled(&mut self, enabled: bool) {
        self.igrpen1 = enabled;
    }

    pub fn group1_enabled(&self) -> bool {
        self.igrpen1
    }

    pub fn write_cntv_ctl(&mut self, value: u64) {
        self.cntv_ctl = value & (CNTV_CTL_ENABLE | CNTV_CTL_IMASK);
        self.timer_fired = false;
    }

    pub fn write_cntv_cval(&mut self, value: u64) {
        self.cntv_cval = value;
        self.timer_fired = false;
    }

    pub fn read_cntv_ctl(&self, now_vns: u64) -> u64 {
        let mut v = self.cntv_ctl;
        if self.cntv_ctl & CNTV_CTL_ENABLE != 0 && self.deadline_vns().is_some_and(|d| now_vns >= d)
        {
            v |= 1 << 2;
        }
        v
    }

    pub fn read_cntv_cval(&self) -> u64 {
        self.cntv_cval
    }

    pub fn next_timer_deadline(&self) -> Option<u64> {
        if self.cntv_ctl & CNTV_CTL_ENABLE == 0
            || self.cntv_ctl & CNTV_CTL_IMASK != 0
            || self.timer_fired
        {
            return None;
        }
        self.deadline_vns()
    }

    fn deadline_vns(&self) -> Option<u64> {
        let ns = (u128::from(self.cntv_cval) * 1_000_000_000).div_ceil(u128::from(self.timer_hz));
        u64::try_from(ns).ok()
    }

    pub fn armed_timer_deliverable(&self) -> bool {
        self.next_timer_deadline().is_some() && self.input_deliverable(self.timer_intid)
    }

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
            line_level: self.line_level,
            priority: self.priority,
            pmr: self.pmr,
            igrpen1: self.igrpen1,
            cntv_ctl: self.cntv_ctl,
            cntv_cval: self.cntv_cval,
            timer_fired: self.timer_fired,
        }
    }

    pub fn restore(state: &GicState, now_vns: u64) -> Result<Gicv3, GicError> {
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
            for file in [
                &state.group,
                &state.enable,
                &state.pending,
                &state.active,
                &state.line_level,
            ] {
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
        g.line_level = state.line_level;
        g.priority = state.priority;
        g.pmr = state.pmr;
        g.igrpen1 = state.igrpen1;
        g.cntv_ctl = state.cntv_ctl;
        g.cntv_cval = state.cntv_cval;
        g.timer_fired = state.timer_fired;
        if g.timer_fired {
            let armed = g.cntv_ctl & CNTV_CTL_ENABLE != 0 && g.cntv_ctl & CNTV_CTL_IMASK == 0;
            let past = g.deadline_vns().is_some_and(|d| d <= now_vns);
            if !(armed && past) {
                return Err(GicError::InvalidState);
            }
        }
        Ok(g)
    }
}

fn word_index(offset: u64, base: u64) -> Option<usize> {
    if (base..base + 4 * BITMAP_WORDS as u64).contains(&offset) {
        Some(((offset - base) / 4) as usize)
    } else {
        None
    }
}

fn bitmap_contains(bitmap: &[u32; BITMAP_WORDS], intid: u32) -> bool {
    bitmap[(intid / 32) as usize] & (1 << (intid % 32)) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gic() -> Gicv3 {
        Gicv3::new(GicConfig {
            impl_spis: 64,
            timer_hz: 62_500_000,
            timer_intid: 27,
        })
        .unwrap()
    }

    fn arm(g: &mut Gicv3, intid: u32, prio: u8) {
        g.mmio_write(GicFrame::Dist, GICD_CTLR, GICD_CTLR_ENABLE_GRP1, 0)
            .unwrap();
        g.set_group1_enabled(true);
        g.set_pmr(0xFF);
        let (w, b) = (intid / 32, intid % 32);
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
        assert_eq!(g.peek_interrupt(), None);
    }

    #[test]
    fn reset_register_files_match_the_stock_kvm_vgicv3() {
        let g = gic();
        let state = g.snapshot();
        assert_eq!(state.group[..3], [u32::MAX; 3]);
        assert!(state.group[3..].iter().all(|&word| word == 0));
        assert_eq!(state.enable[0], u32::from(u16::MAX));
        assert!(state.enable[1..].iter().all(|&word| word == 0));
    }

    #[test]
    fn fixed_register_surface_matches_the_stock_kvm_vgicv3() {
        let mut g = gic();
        assert_eq!(g.mmio_read(GicFrame::Dist, GICD_CTLR, 0).unwrap(), 0x50);
        g.mmio_write(GicFrame::Dist, GICD_CTLR, u32::MAX, 0)
            .unwrap();
        assert_eq!(g.mmio_read(GicFrame::Dist, GICD_CTLR, 0).unwrap(), 0x52);
        assert_eq!(g.snapshot().gicd_ctlr, 0x02);

        let mut invalid = g.snapshot();
        invalid.gicd_ctlr |= 1;
        assert!(matches!(
            Gicv3::restore(&invalid, 0),
            Err(GicError::InvalidState)
        ));

        let ctlr = g.mmio_read(GicFrame::Redist, GICR_CTLR, 0).unwrap();
        let typer = g.mmio_read(GicFrame::Redist, GICR_TYPER_LO, 0).unwrap();
        let planted_typer = GICR_TYPER_LAST | (1 << 3);
        assert_eq!(ctlr, GICR_CTLR_IR);
        assert_eq!(typer, 0x10, "GICR_TYPER.Last is bit 4");
        assert_eq!(typer, GICR_TYPER_LAST);
        assert_ne!(typer, planted_typer);
        assert!(ctlr & GICR_CTLR_IR != 0 || typer & (1 << 3) != 0);
        assert_eq!(g.mmio_read(GicFrame::Redist, GICR_TYPER_HI, 0).unwrap(), 0);
        assert_eq!(g.mmio_read(GicFrame::Dist, GICD_PIDR2, 0).unwrap(), 0x30);
        assert_eq!(g.mmio_read(GicFrame::Redist, GICR_IIDR, 0).unwrap(), 0);
        assert_eq!(g.mmio_read(GicFrame::Redist, GICR_PIDR2, 0).unwrap(), 0x30);
    }

    #[test]
    fn line_inputs_and_pulses_preserve_exact_bitmap_identity() {
        let mut g = gic();
        arm(&mut g, 65, 0x20);

        g.assert_line(65).unwrap();
        assert_eq!(g.snapshot().line_level[..3], [0, 0, 2]);
        assert_eq!(g.peek_interrupt(), Some(65));
        g.deassert_line(65).unwrap();
        assert_eq!(g.snapshot().line_level[..3], [0, 0, 0]);
        assert_eq!(g.peek_interrupt(), None);

        g.pulse(65).unwrap();
        assert_eq!(g.snapshot().pending[..3], [0, 0, 2]);
        assert_eq!(g.peek_interrupt(), Some(65));
        assert_eq!(g.assert_line(96), Err(GicError::BadIntId(96)));
        assert_eq!(g.deassert_line(96), Err(GicError::BadIntId(96)));
        assert_eq!(g.pulse(96), Err(GicError::BadIntId(96)));
    }

    #[test]
    fn pending_and_level_inputs_are_combined_without_cancellation() {
        let mut g = gic();
        arm(&mut g, 40, 0x20);
        g.raise(40).unwrap();
        g.assert_line(40).unwrap();
        assert_eq!(g.peek_interrupt(), Some(40));
    }

    #[test]
    fn every_input_delivery_gate_is_independently_observable() {
        let mut g = gic();
        assert!(!g.group1_enabled());
        arm(&mut g, 40, 0x40);
        assert!(g.group1_enabled());
        assert!(g.input_deliverable(40));

        g.set_group1_enabled(false);
        assert!(!g.group1_enabled());
        assert!(!g.input_deliverable(40));
        g.raise(40).unwrap();
        assert_eq!(g.peek_interrupt(), None);
        g.set_group1_enabled(true);
        assert_eq!(g.peek_interrupt(), Some(40));
        g.lower(40).unwrap();

        g.mmio_write(GicFrame::Dist, GICD_CTLR, 0, 0).unwrap();
        assert!(!g.input_deliverable(40));
        g.raise(40).unwrap();
        assert_eq!(g.peek_interrupt(), None);
        g.mmio_write(GicFrame::Dist, GICD_CTLR, GICD_CTLR_ENABLE_GRP1, 0)
            .unwrap();
        assert_eq!(g.peek_interrupt(), Some(40));
        g.lower(40).unwrap();

        g.mmio_write(GicFrame::Dist, ICENABLER_BASE + 4, 1 << 8, 0)
            .unwrap();
        assert!(!g.input_deliverable(40));
        g.mmio_write(GicFrame::Dist, ISENABLER_BASE + 4, 1 << 8, 0)
            .unwrap();

        g.mmio_write(GicFrame::Dist, IGROUPR_BASE + 4, 0, 0)
            .unwrap();
        assert!(!g.input_deliverable(40));
        g.mmio_write(GicFrame::Dist, IGROUPR_BASE + 4, 1 << 8, 0)
            .unwrap();

        g.set_pmr(0x40);
        assert!(!g.input_deliverable(40));
        g.set_pmr(0x41);
        assert!(g.input_deliverable(40));

        arm(&mut g, 41, 0x20);
        g.raise(41).unwrap();
        assert_eq!(g.take_interrupt(), Some(41));
        arm(&mut g, 40, 0x20);
        assert!(!g.input_deliverable(40));
        arm(&mut g, 40, 0x10);
        assert!(g.input_deliverable(40));

        let mut bitmap = [0_u32; BITMAP_WORDS];
        bitmap[2] = 1 << 1;
        assert!(bitmap_contains(&bitmap, 65));
        assert!(!bitmap_contains(&bitmap, 64));
        assert!(!bitmap_contains(&bitmap, 66));
    }

    #[test]
    fn active_interrupt_scans_every_bitmap_word_and_breaks_ties() {
        let mut g = gic();
        arm(&mut g, 64, 0x40);
        g.raise(64).unwrap();
        assert_eq!(g.take_interrupt(), Some(64));
        arm(&mut g, 65, 0x20);
        g.raise(65).unwrap();
        assert_eq!(g.take_interrupt(), Some(65));
        assert_eq!(g.active_interrupt(), Some(65));
        g.eoi(65).unwrap();
        assert_eq!(g.active_interrupt(), Some(64));
    }

    #[test]
    fn sgi_zero_delivers_when_programmed() {
        let mut g = gic();
        arm(&mut g, 0, 0x40);
        g.raise(0).unwrap();
        assert_eq!(g.peek_interrupt(), Some(0));
    }

    #[test]
    fn arbitration_picks_highest_priority_then_lowest_intid() {
        let mut g = gic();
        arm(&mut g, 40, 0x80);
        arm(&mut g, 41, 0x40);
        arm(&mut g, 42, 0x40);
        g.raise(40).unwrap();
        g.raise(42).unwrap();
        g.raise(41).unwrap();
        assert_eq!(g.peek_interrupt(), Some(41));
        assert_eq!(g.take_interrupt(), Some(41));
        assert_eq!(g.peek_interrupt(), None);
        g.eoi(41).unwrap();
        assert_eq!(g.take_interrupt(), Some(42));
        g.eoi(42).unwrap();
        assert_eq!(g.take_interrupt(), Some(40));
    }

    #[test]
    fn lowering_a_level_clears_pending_but_not_active() {
        let mut g = gic();
        arm(&mut g, 20, 0x40);
        g.raise(20).unwrap();
        assert_eq!(g.peek_interrupt(), Some(20));
        g.lower(20).unwrap();
        assert_eq!(g.peek_interrupt(), None);

        g.raise(20).unwrap();
        assert_eq!(g.take_interrupt(), Some(20));
        g.lower(20).unwrap();
        assert_eq!(g.active_interrupt(), Some(20));
        g.eoi(20).unwrap();
        assert_eq!(g.active_interrupt(), None);
        assert_eq!(g.lower(96), Err(GicError::BadIntId(96)));
    }

    #[test]
    fn pmr_masks_delivery() {
        let mut g = gic();
        arm(&mut g, 40, 0x80);
        g.raise(40).unwrap();
        g.set_pmr(0x80);
        assert_eq!(g.peek_interrupt(), None);
        g.set_pmr(0x81);
        assert_eq!(g.peek_interrupt(), Some(40));
    }

    #[test]
    fn unimplemented_intids_are_rejected_and_writes_masked() {
        let mut g = gic();
        assert_eq!(g.raise(96), Err(GicError::BadIntId(96)));
        assert_eq!(g.raise(1023), Err(GicError::BadIntId(1023)));
        assert!(g.raise(95).is_ok());
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
        g.write_cntv_cval(125);
        g.write_cntv_ctl(CNTV_CTL_ENABLE);
        assert_eq!(g.next_timer_deadline(), Some(2000));
        assert!(g.armed_timer_deliverable());
        assert!(g.input_deliverable(27));
        assert!(!g.advance_to(1999));
        assert!(g.advance_to(2000));
        assert_eq!(g.peek_interrupt(), Some(27));
        assert!(g.input_deliverable(27));
        assert_eq!(g.next_timer_deadline(), None);
        assert!(!g.advance_to(3000));
        g.write_cntv_cval(250);
        assert_eq!(g.next_timer_deadline(), Some(4000));
    }

    #[test]
    fn timer_delivery_requires_both_a_deadline_and_an_open_input() {
        let mut g = gic();
        g.write_cntv_cval(125);
        g.write_cntv_ctl(CNTV_CTL_ENABLE);
        assert_eq!(g.next_timer_deadline(), Some(2000));
        assert!(!g.armed_timer_deliverable());

        arm(&mut g, 27, 0x20);
        assert!(g.armed_timer_deliverable());
        g.write_cntv_ctl(0);
        assert!(g.input_deliverable(27));
        assert!(!g.armed_timer_deliverable());
    }

    #[test]
    fn input_deliverability_observes_active_and_priority_state() {
        let mut g = gic();
        assert!(!g.input_deliverable(40));
        assert!(!g.input_deliverable(96));
        arm(&mut g, 40, 0x80);
        assert!(g.input_deliverable(40));
        g.set_pmr(0x80);
        assert!(!g.input_deliverable(40));
        g.set_pmr(0x81);
        g.raise(40).unwrap();
        assert_eq!(g.take_interrupt(), Some(40));
        assert!(!g.input_deliverable(40));
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
        let r = Gicv3::restore(&s, 5000).unwrap();
        assert_eq!(r.snapshot(), s);
        assert_eq!(r.peek_interrupt(), g.peek_interrupt());
    }

    #[test]
    fn restore_rejects_an_unreachable_timer_latch() {
        let mut g = gic();
        g.write_cntv_cval(125);
        g.write_cntv_ctl(CNTV_CTL_ENABLE);
        g.advance_to(5000);
        let good = g.snapshot();
        assert!(good.timer_fired);
        assert!(Gicv3::restore(&good, 5000).is_ok());
        assert!(Gicv3::restore(&good, 2000).is_ok());
        assert_eq!(
            Gicv3::restore(&good, 1999).unwrap_err(),
            GicError::InvalidState
        );

        let mut forged = gic().snapshot();
        forged.cntv_ctl = CNTV_CTL_ENABLE;
        forged.cntv_cval = 125;
        forged.timer_fired = true;
        let (w, b) = ((forged.timer_intid / 32) as usize, forged.timer_intid % 32);
        assert_eq!(forged.pending[w] & (1 << b), 0, "...with no pending PPI");
        assert_eq!(
            Gicv3::restore(&forged, 1000).unwrap_err(),
            GicError::InvalidState
        );

        let mut disabled = good.clone();
        disabled.cntv_ctl = 0;
        assert_eq!(
            Gicv3::restore(&disabled, u64::MAX).unwrap_err(),
            GicError::InvalidState
        );
        let mut masked = good;
        masked.cntv_ctl = CNTV_CTL_ENABLE | CNTV_CTL_IMASK;
        assert_eq!(
            Gicv3::restore(&masked, u64::MAX).unwrap_err(),
            GicError::InvalidState
        );
    }

    #[test]
    fn config_reports_the_construction_parameters() {
        let cfg = GicConfig {
            impl_spis: 64,
            timer_hz: 62_500_000,
            timer_intid: 27,
        };
        let g = Gicv3::new(cfg).unwrap();
        assert_eq!(g.config(), cfg);
        assert_eq!(Gicv3::restore(&g.snapshot(), 0).unwrap().config(), cfg);
    }

    #[test]
    fn restore_rejects_state_past_the_implemented_range() {
        let g = gic();
        let mut s = g.snapshot();
        s.pending[4] = 1;
        assert_eq!(Gicv3::restore(&s, 0).unwrap_err(), GicError::InvalidState);
        let mut s = g.snapshot();
        s.priority[96] = 1;
        assert_eq!(Gicv3::restore(&s, 0).unwrap_err(), GicError::InvalidState);
        let mut s = g.snapshot();
        s.version = 99;
        assert_eq!(Gicv3::restore(&s, 0).unwrap_err(), GicError::InvalidState);
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
        assert_eq!(g.mmio_read(GicFrame::Dist, 0x0C00, 0).unwrap(), 0);
        g.mmio_write(GicFrame::Dist, 0x0C00, 0xFFFF_FFFF, 0)
            .unwrap();
        assert_eq!(g.mmio_read(GicFrame::Dist, 0x0C00, 0).unwrap(), 0);
        let typer = g.mmio_read(GicFrame::Dist, GICD_TYPER, 0).unwrap();
        assert_eq!(typer & 0x1F, 2);
        assert_eq!(
            g.mmio_read(GicFrame::Dist, GICD_PIDR2, 0).unwrap(),
            GIC_PIDR2_ARCH_GICV3
        );
        assert_eq!(
            g.mmio_read(GicFrame::Redist, GICR_PIDR2, 0).unwrap(),
            GIC_PIDR2_ARCH_GICV3
        );
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
