// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::error::LapicError;
use crate::state::{
    APIC_DFR, APIC_EOI, APIC_ESR, APIC_ICR_HIGH, APIC_ICR_LOW, APIC_ID, APIC_IRR, APIC_ISR,
    APIC_LDR, APIC_LVT_TIMER, APIC_MAX_OFFSET, APIC_PPR, APIC_SVR, APIC_TDCR, APIC_TMCCT,
    APIC_TMICT, APIC_TMR, APIC_TPR, APIC_VERSION, APIC_VERSION_VALUE, LAPIC_STATE_VERSION,
    LapicState,
};

const NS_PER_SEC: u128 = 1_000_000_000;

const LVT_TIMER: usize = 0;

const LVT_RESET: u32 = 0x0001_0000;

const LVT_MASK_BIT: u32 = 1 << 16;

const SVR_ENABLE_BIT: u32 = 1 << 8;

const SVR_RESET: u32 = 0x0000_00FF;

const SVR_WRITE_MASK: u32 = 0x0000_13FF;

const ICR_LOW_WRITE_MASK: u32 = 0x000C_CFFF;

const ESR_SEND_ILLEGAL_VECTOR: u32 = 1 << 5;

const PHYSICAL_BROADCAST: u32 = 0xFF;

const ID_VALID_MASK: u32 = 0xFF00_0000;
const TPR_WRITE_MASK: u32 = 0x0000_00FF;
const LDR_WRITE_MASK: u32 = 0xFF00_0000;
const DFR_WRITE_MASK: u32 = 0xF000_0000;
const DFR_RESERVED_ONES: u32 = 0x0FFF_FFFF;
const ESR_VALID_MASK: u32 = ESR_SEND_ILLEGAL_VECTOR;
const ICR_HIGH_WRITE_MASK: u32 = 0xFF00_0000;
const TDCR_WRITE_MASK: u32 = 0x0000_000B;

const LVT_TIMER_MASK: u32 = 0x0007_00FF;
const LVT_LOCAL_MASK: u32 = 0x0001_07FF;
const LVT_LINT_MASK: u32 = 0x0001_A7FF;
const LVT_ERROR_MASK: u32 = 0x0001_00FF;

const TIMER_ONESHOT: u32 = 0b00;
const TIMER_PERIODIC: u32 = 0b01;

const ID_SHIFT: u32 = 24;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LapicConfig {
    pub apic_id: u32,
    pub timer_hz: u64,
}

#[derive(Clone, Debug)]
pub struct Lapic {
    id: u32,
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
    lvt: [u32; 6],
    initial_count: u32,
    count_at_arm: u32,
    timer_arm_vns: u64,
    timer_running: bool,
    timer_pending: bool,
}

impl Lapic {
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
            _ => 0,
        };
        Ok(value)
    }

    pub fn mmio_write(&mut self, offset: u32, value: u32, now_vns: u64) -> Result<(), LapicError> {
        check_offset(offset)?;
        match offset {
            APIC_TPR => self.tpr = value & TPR_WRITE_MASK,
            APIC_EOI => self.eoi(),
            APIC_LDR => self.ldr = value & LDR_WRITE_MASK,
            APIC_DFR => self.dfr = (value & DFR_WRITE_MASK) + DFR_RESERVED_ONES,
            APIC_SVR => self.timer_config_write(now_vns, |s| s.svr = value & SVR_WRITE_MASK),
            APIC_ESR => self.esr = 0,
            APIC_ICR_LOW => self.write_icr_low(value),
            APIC_ICR_HIGH => self.icr_high = value & ICR_HIGH_WRITE_MASK,
            APIC_TMICT => self.write_initial_count(value, now_vns),
            APIC_TDCR => {
                self.timer_config_write(now_vns, |s| s.divide_config = value & TDCR_WRITE_MASK)
            }
            0x320..=0x370 => {
                let idx = ((offset - APIC_LVT_TIMER) >> 4) as usize;
                if idx == LVT_TIMER {
                    self.timer_config_write(now_vns, |s| s.lvt[LVT_TIMER] = value & LVT_TIMER_MASK);
                } else {
                    self.lvt[idx] = value & lvt_write_mask(idx);
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub fn next_timer_deadline(&self) -> Option<u64> {
        if !self.timer_active() {
            return None;
        }
        let deadline = u128::from(self.timer_arm_vns) + self.period_for(self.count_at_arm);
        u64::try_from(deadline).ok()
    }

    pub fn advance_to(&mut self, now_vns: u64) -> bool {
        if !self.timer_active() {
            return false;
        }
        let seg_period = self.period_for(self.count_at_arm);
        let elapsed = u128::from(now_vns.saturating_sub(self.timer_arm_vns));
        if elapsed < seg_period {
            return false;
        }
        let vector = self.timer_vector();
        if self.timer_mode() == TIMER_PERIODIC {
            let full = self.period_for(self.initial_count);
            let after_first = u128::from(self.timer_arm_vns) + seg_period;
            let extra = u128::from(now_vns).saturating_sub(after_first);
            let k = extra / full;
            self.timer_arm_vns = sat_u64(after_first + k * full);
            self.count_at_arm = self.initial_count;
        } else {
            self.timer_running = false;
            self.timer_pending = false;
        }
        set_vec(&mut self.irr, vector);
        true
    }

    pub fn raise(&mut self, vector: u8) -> Result<(), LapicError> {
        if vector < 16 {
            return Err(LapicError::ReservedVector(vector));
        }
        set_vec(&mut self.irr, vector);
        Ok(())
    }

    pub fn has_deliverable(&self) -> bool {
        if !self.apic_enabled() {
            return false;
        }
        match highest_vec(&self.irr) {
            Some(v) => priority_class(v) > (self.ppr() >> 4),
            None => false,
        }
    }

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

    pub fn armed_timer_deliverable(&self) -> bool {
        self.timer_active()
            && self.timer_vector() >= 16
            && priority_class(self.timer_vector()) > (self.ppr() >> 4)
    }

    pub fn take_interrupt(&mut self) -> Option<u8> {
        let v = self.peek_interrupt()?;
        clear_vec(&mut self.irr, v);
        set_vec(&mut self.isr, v);
        Some(v)
    }

    pub fn eoi(&mut self) {
        if let Some(v) = highest_vec(&self.isr) {
            clear_vec(&mut self.isr, v);
        }
    }

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
        if lapic.timer_pending && lapic.initial_count == 0 {
            return Err(LapicError::InvalidState);
        }
        if lapic.timer_running != lapic.timer_armable() {
            return Err(LapicError::InvalidState);
        }
        if lapic.timer_running && lapic.count_at_arm > lapic.initial_count {
            return Err(LapicError::InvalidState);
        }
        Ok(lapic)
    }

    fn apic_enabled(&self) -> bool {
        self.svr & SVR_ENABLE_BIT != 0
    }

    fn ppr(&self) -> u32 {
        let tpr = self.tpr & 0xFF;
        let isrv = highest_vec(&self.isr).map(u32::from).unwrap_or(0);
        if (tpr >> 4) >= (isrv >> 4) {
            tpr
        } else {
            isrv & 0xF0
        }
    }

    fn timer_mode(&self) -> u32 {
        (self.lvt[LVT_TIMER] >> 17) & 0b11
    }

    pub fn timer_vector(&self) -> u8 {
        (self.lvt[LVT_TIMER] & 0xFF) as u8
    }

    fn timer_masked(&self) -> bool {
        self.lvt[LVT_TIMER] & LVT_MASK_BIT != 0
    }

    fn timer_active(&self) -> bool {
        self.timer_running
            && self.apic_enabled()
            && !self.timer_masked()
            && matches!(self.timer_mode(), TIMER_ONESHOT | TIMER_PERIODIC)
    }

    fn period_for(&self, count: u32) -> u128 {
        let divide = divide_value(self.divide_config);
        let numer = u128::from(count) * u128::from(divide) * NS_PER_SEC;
        numer.div_ceil(u128::from(self.timer_hz))
    }

    fn elapsed_ticks(&self, delta: u64) -> u32 {
        let divide = divide_value(self.divide_config);
        let ticks =
            (u128::from(delta) * u128::from(self.timer_hz)) / (u128::from(divide) * NS_PER_SEC);
        sat_u32(ticks)
    }

    fn remaining_at(&self, now_vns: u64) -> u32 {
        let elapsed = now_vns.saturating_sub(self.timer_arm_vns);
        self.count_at_arm
            .saturating_sub(self.elapsed_ticks(elapsed))
    }

    fn current_count(&self, now_vns: u64) -> u32 {
        if self.timer_running {
            self.remaining_at(now_vns)
        } else {
            0
        }
    }

    fn timer_armable(&self) -> bool {
        self.timer_pending
            && self.apic_enabled()
            && !self.timer_masked()
            && matches!(self.timer_mode(), TIMER_ONESHOT | TIMER_PERIODIC)
    }

    fn write_initial_count(&mut self, value: u32, now_vns: u64) {
        self.initial_count = value;
        self.timer_pending = value != 0;
        self.retime(now_vns, None, divide_value(self.divide_config));
    }

    fn running_remaining(&self, now_vns: u64) -> Option<u32> {
        self.timer_running.then(|| self.remaining_at(now_vns))
    }

    fn timer_config_write(&mut self, now_vns: u64, apply: impl FnOnce(&mut Self)) {
        let prior_remaining = self.running_remaining(now_vns);
        let old_divide = divide_value(self.divide_config);
        apply(self);
        self.retime(now_vns, prior_remaining, old_divide);
    }

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
            Some(_) => {}
            None => {
                self.count_at_arm = self.initial_count;
                self.timer_arm_vns = now_vns;
            }
        }
        self.timer_running = true;
    }

    fn write_icr_low(&mut self, value: u32) {
        self.icr_low = value & ICR_LOW_WRITE_MASK;
        let delivery_mode = (value >> 8) & 0b111;
        let shorthand = (value >> 18) & 0b11;
        let physical_dest = (value >> 11) & 1 == 0;
        let vector = (value & 0xFF) as u8;

        let self_target = match shorthand {
            0b01 | 0b10 => true,
            0b00 => {
                let dest = (self.icr_high >> 24) & 0xFF;
                let apic_id = (self.id >> 24) & 0xFF;
                physical_dest && (dest == apic_id || dest == PHYSICAL_BROADCAST)
            }
            _ => false,
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

fn check_offset(offset: u32) -> Result<(), LapicError> {
    if offset & 0xF != 0 || offset > APIC_MAX_OFFSET {
        return Err(LapicError::BadOffset(offset));
    }
    Ok(())
}

fn divide_value(tdcr: u32) -> u64 {
    let sel = ((tdcr & 0b1000) >> 1) + (tdcr & 0b11);
    if sel == 0b111 { 1 } else { 2u64 << sel }
}

fn lvt_write_mask(index: usize) -> u32 {
    match index {
        0 => LVT_TIMER_MASK,
        1 | 2 => LVT_LOCAL_MASK,
        3 | 4 => LVT_LINT_MASK,
        _ => LVT_ERROR_MASK,
    }
}

fn state_bits_canonical(state: &LapicState) -> bool {
    let registers_ok = state.id & !ID_VALID_MASK == 0
        && state.tpr & !TPR_WRITE_MASK == 0
        && state.svr & !SVR_WRITE_MASK == 0
        && state.ldr & !LDR_WRITE_MASK == 0
        && state.dfr & DFR_RESERVED_ONES == DFR_RESERVED_ONES
        && state.esr & !ESR_VALID_MASK == 0
        && state.icr_low & !ICR_LOW_WRITE_MASK == 0
        && state.icr_high & !ICR_HIGH_WRITE_MASK == 0
        && state.divide_config & !TDCR_WRITE_MASK == 0;
    let lvt_ok = state.lvt[0] & !lvt_write_mask(0) == 0
        && state.lvt[1] & !lvt_write_mask(1) == 0
        && state.lvt[2] & !lvt_write_mask(2) == 0
        && state.lvt[3] & !lvt_write_mask(3) == 0
        && state.lvt[4] & !lvt_write_mask(4) == 0
        && state.lvt[5] & !lvt_write_mask(5) == 0;
    registers_ok && lvt_ok
}

fn priority_class(vector: u8) -> u32 {
    u32::from(vector) >> 4
}

fn set_vec(bits: &mut [u32; 8], vector: u8) {
    bits[(vector >> 5) as usize] |= 1u32 << (vector & 31);
}

fn clear_vec(bits: &mut [u32; 8], vector: u8) {
    bits[(vector >> 5) as usize] &= !(1u32 << (vector & 31));
}

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

fn sat_u64(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn sat_u32(value: u128) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(kani)]
#[path = "device_proofs.rs"]
mod proofs;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lapic_register_bits_match_the_architecture() {
        assert_eq!(LVT_MASK_BIT, 0x1_0000, "LVT Mask is bit 16");
        assert_eq!(SVR_ENABLE_BIT, 0x100, "SVR APIC Software Enable is bit 8");
        assert_eq!(
            ESR_SEND_ILLEGAL_VECTOR, 0x20,
            "ESR Send Illegal Vector is bit 5"
        );
    }

    fn enabled(timer_hz: u64) -> Lapic {
        let mut l = Lapic::new(LapicConfig {
            apic_id: 0,
            timer_hz,
        })
        .expect("valid config");
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
        assert_eq!(l.mmio_read(0x004, 0), Err(LapicError::BadOffset(0x004)));
        assert_eq!(l.mmio_read(0x1000, 0), Err(LapicError::BadOffset(0x1000)));
        assert_eq!(
            l.mmio_read(APIC_MAX_OFFSET + 0x10, 0),
            Err(LapicError::BadOffset(APIC_MAX_OFFSET + 0x10))
        );
        assert_eq!(l.mmio_read(APIC_MAX_OFFSET, 0), Ok(0));
    }

    #[test]
    fn version_register_is_fixed() {
        let l = enabled(25_000_000);
        assert_eq!(l.mmio_read(APIC_VERSION, 0), Ok(APIC_VERSION_VALUE));
    }

    #[test]
    fn armed_timer_deliverable_gates_on_active_vector_and_priority() {
        let mut l = enabled(24_000_000);
        assert!(!l.armed_timer_deliverable(), "no timer armed");

        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert!(l.armed_timer_deliverable(), "armed, valid vector, low TPR");

        l.mmio_write(APIC_LVT_TIMER, 16, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert!(
            l.armed_timer_deliverable(),
            "vector 16 is deliverable (>= 16)"
        );

        l.mmio_write(APIC_LVT_TIMER, 15, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert!(
            !l.armed_timer_deliverable(),
            "reserved vector < 16 never delivers"
        );

        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        l.mmio_write(APIC_TPR, 0x40, 0).unwrap();
        assert!(
            !l.armed_timer_deliverable(),
            "equal priority class is not deliverable"
        );
        l.mmio_write(APIC_TPR, 0x30, 0).unwrap();
        assert!(
            l.armed_timer_deliverable(),
            "vector class outranks TPR class"
        );

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
        assert_eq!(l.mmio_write(APIC_VERSION, 0xDEAD_BEEF, 0), Ok(()));
        assert_eq!(l.mmio_read(APIC_VERSION, 0), Ok(APIC_VERSION_VALUE));
        assert_eq!(l.mmio_write(APIC_PPR, 0xFF, 0), Ok(()));
        assert_eq!(l.mmio_write(APIC_TMCCT, 0xFF, 0), Ok(()));
        assert_eq!(l.mmio_write(APIC_IRR, 0xFF, 0), Ok(()));
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
            assert_eq!(l.mmio_read(APIC_TDCR, 0), Ok(tdcr & 0xB));
        }
        assert_eq!(divide_value(0b0000), 2);
        assert_eq!(divide_value(0b0100), 2);
        assert_eq!(divide_value(0b0001), 4);
        assert_eq!(divide_value(0b0011), 16);
        assert_eq!(divide_value(0b1000), 32);
        assert_eq!(divide_value(0b1010), 128);
        assert_eq!(divide_value(0b1011), 1);
        assert_eq!(divide_value(0b1111), 1);
    }

    #[test]
    fn tdcr_bit2_dropped_not_stored() {
        for base in 0u32..=0xF {
            let with_bit2 = base | 0b100;
            let without = base & !0b100;

            let mut a = enabled(24_000_000);
            let mut b = enabled(24_000_000);
            a.mmio_write(APIC_TDCR, with_bit2, 0).unwrap();
            b.mmio_write(APIC_TDCR, without, 0).unwrap();

            assert_eq!(a.mmio_read(APIC_TDCR, 0), Ok(without));
            assert_eq!(a.mmio_read(APIC_TDCR, 0), b.mmio_read(APIC_TDCR, 0));

            a.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
            b.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
            a.mmio_write(APIC_TMICT, 1000, 0).unwrap();
            b.mmio_write(APIC_TMICT, 1000, 0).unwrap();
            assert_eq!(a.next_timer_deadline(), b.next_timer_deadline());
            assert_eq!(a.mmio_read(APIC_TMCCT, 1234), b.mmio_read(APIC_TMCCT, 1234));

            assert_eq!(a.snapshot(), b.snapshot());
        }

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
        let icr = 0x40 | (0b01 << 18);
        l.mmio_write(APIC_ICR_LOW, icr, 0).expect("icr write");
        assert!(l.has_deliverable());
        assert_eq!(l.take_interrupt(), Some(0x40));
    }

    #[test]
    fn all_including_self_ipi_raises_vector() {
        let mut l = enabled(25_000_000);
        let icr = 0x50 | (0b10 << 18);
        l.mmio_write(APIC_ICR_LOW, icr, 0).expect("icr write");
        assert_eq!(l.take_interrupt(), Some(0x50));
    }

    #[test]
    fn all_excluding_self_ipi_is_noop() {
        let mut l = enabled(25_000_000);
        let icr = 0x60 | (0b11 << 18);
        l.mmio_write(APIC_ICR_LOW, icr, 0).expect("icr write");
        assert!(!l.has_deliverable());
        assert_eq!(l.take_interrupt(), None);
    }

    #[test]
    fn physical_self_ipi_matches_apic_id() {
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_ICR_HIGH, 0, 0).expect("icr-high");
        l.mmio_write(APIC_ICR_LOW, 0x60, 0).expect("icr-low");
        assert_eq!(l.take_interrupt(), Some(0x60));

        let mut b = enabled(25_000_000);
        b.mmio_write(APIC_ICR_HIGH, PHYSICAL_BROADCAST << 24, 0)
            .unwrap();
        b.mmio_write(APIC_ICR_LOW, 0x61, 0).unwrap();
        assert_eq!(b.take_interrupt(), Some(0x61));
    }

    #[test]
    fn physical_non_matching_destination_is_noop() {
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_ICR_HIGH, 5 << 24, 0).expect("icr-high");
        l.mmio_write(APIC_ICR_LOW, 0x60, 0).expect("icr-low");
        assert_eq!(l.take_interrupt(), None);
    }

    #[test]
    fn reserved_vector_self_ipi_sets_esr() {
        let mut l = enabled(25_000_000);
        let icr = 0x0F | (0b01 << 18);
        l.mmio_write(APIC_ICR_LOW, icr, 0).expect("icr write");
        assert!(!l.has_deliverable());
        assert_eq!(l.mmio_read(APIC_ESR, 0), Ok(0x20));
        l.mmio_write(APIC_ESR, 0, 0).expect("esr write");
        assert_eq!(l.mmio_read(APIC_ESR, 0), Ok(0));
    }

    #[test]
    fn non_fixed_delivery_mode_self_ipi_is_noop() {
        let mut l = enabled(25_000_000);
        let icr = 0x40 | (0b100 << 8) | (0b01 << 18);
        l.mmio_write(APIC_ICR_LOW, icr, 0).expect("icr write");
        assert!(!l.has_deliverable());
        assert_eq!(l.take_interrupt(), None);
    }

    #[test]
    fn logical_mode_no_shorthand_ipi_is_noop() {
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_ICR_HIGH, 0, 0).expect("icr-high");
        let icr = 0x40 | (1 << 11);
        l.mmio_write(APIC_ICR_LOW, icr, 0).expect("icr-low");
        assert_eq!(l.take_interrupt(), None);
    }

    #[test]
    fn physical_self_ipi_matches_nonzero_apic_id() {
        let mut l = Lapic::new(LapicConfig {
            apic_id: 0x12,
            timer_hz: 25_000_000,
        })
        .unwrap();
        l.mmio_write(APIC_SVR, SVR_RESET | SVR_ENABLE_BIT, 0)
            .unwrap();
        l.mmio_write(APIC_ICR_HIGH, 0x12 << 24, 0).unwrap();
        l.mmio_write(APIC_ICR_LOW, 0x60, 0).unwrap();
        assert_eq!(l.take_interrupt(), Some(0x60));
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
        l.eoi();
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
        l.mmio_write(APIC_ID, 7 << 24, 0).unwrap();
        assert_eq!(l.mmio_read(APIC_ID, 0), Ok(3 << 24));
    }

    #[test]
    fn stateful_registers_round_trip_through_mmio() {
        use crate::state::{
            APIC_LVT_ERROR, APIC_LVT_LINT0, APIC_LVT_LINT1, APIC_LVT_PERFMON, APIC_LVT_THERMAL,
        };
        let cases: &[(u32, u32, u32)] = &[
            (APIC_TPR, 0xFFFF_FFFF, 0x0000_00FF),
            (APIC_LDR, 0xFFFF_FFFF, 0xFF00_0000),
            (APIC_DFR, 0x0000_000F, 0x0FFF_FFFF),
            (APIC_SVR, 0xFFFF_FFFF, 0x0000_13FF),
            (APIC_ICR_LOW, 0xFFFF_FFFF, 0x000C_CFFF),
            (APIC_ICR_HIGH, 0xFFFF_FFFF, 0xFF00_0000),
            (APIC_TDCR, 0xFFFF_FFFF, 0x0000_000B),
            (APIC_LVT_TIMER, 0xFFFF_FFFF, 0x0007_00FF),
            (APIC_LVT_THERMAL, 0xFFFF_FFFF, 0x0001_07FF),
            (APIC_LVT_PERFMON, 0xFFFF_FFFF, 0x0001_07FF),
            (APIC_LVT_LINT0, 0xFFFF_FFFF, 0x0001_A7FF),
            (APIC_LVT_LINT1, 0xFFFF_FFFF, 0x0001_A7FF),
            (APIC_LVT_ERROR, 0xFFFF_FFFF, 0x0001_00FF),
            (APIC_TMICT, 0x1234_5678, 0x1234_5678),
        ];
        for &(offset, write, expect) in cases {
            let mut l = enabled(25_000_000);
            let before = l.mmio_read(offset, 0).expect("read");
            assert_ne!(before, expect, "offset {offset:#x} already reads `expect`");
            l.mmio_write(offset, write, 0).expect("write");
            assert_eq!(l.mmio_read(offset, 0), Ok(expect), "offset {offset:#x}");
        }
    }

    #[test]
    fn periodic_advance_idempotent_at_saturated_vtime() {
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_LVT_TIMER, 0x40 | (TIMER_PERIODIC << 17), 0)
            .unwrap();
        let arm = u64::MAX - 10;
        l.mmio_write(APIC_TMICT, 1_000_000, arm).unwrap();
        assert!(!l.advance_to(u64::MAX));
        assert!(!l.has_deliverable());
        let snap = l.snapshot();
        assert!(!l.advance_to(u64::MAX));
        assert_eq!(l.snapshot(), snap);
    }

    #[test]
    fn fired_oneshot_not_rearmed_by_gating_writes() {
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        let deadline = l.next_timer_deadline().expect("armed");
        assert!(l.advance_to(deadline));
        assert_eq!(l.take_interrupt(), Some(0x40));
        l.eoi();
        assert_eq!(l.next_timer_deadline(), None);
        assert_eq!(l.mmio_read(APIC_TMICT, deadline), Ok(1000));

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

        l.mmio_write(APIC_TMICT, 2000, deadline).unwrap();
        assert!(l.next_timer_deadline().is_some());
    }

    #[test]
    fn tdcr_change_midcount_reanchors_not_retroactive() {
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_TDCR, 0b0000, 0).unwrap();
        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert_eq!(l.next_timer_deadline(), Some(80_000));

        assert_eq!(l.mmio_read(APIC_TMCCT, 40_000), Ok(500));

        l.mmio_write(APIC_TDCR, 0b1010, 40_000).unwrap();
        assert_eq!(l.mmio_read(APIC_TMCCT, 40_000), Ok(500));
        assert_eq!(l.next_timer_deadline(), Some(2_600_000));

        assert!(!l.advance_to(40_000));
        assert!(!l.advance_to(2_599_999));
        assert!(l.advance_to(2_600_000));
        assert_eq!(l.take_interrupt(), Some(0x40));
    }

    #[test]
    fn eoi_via_mmio_retires_isr() {
        let mut l = enabled(25_000_000);
        l.raise(0x40).unwrap();
        assert_eq!(l.take_interrupt(), Some(0x40));
        assert_eq!(l.mmio_read(APIC_ISR + 2 * 0x10, 0), Ok(1));
        l.mmio_write(APIC_EOI, 0, 0).unwrap();
        assert_eq!(l.mmio_read(APIC_ISR + 2 * 0x10, 0), Ok(0));
    }

    #[test]
    fn restore_reads_back_isr_tmr_irr_words() {
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
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_LVT_TIMER, 0x40 | LVT_MASK_BIT, 0)
            .unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert_eq!(l.next_timer_deadline(), None);
        assert_eq!(l.mmio_read(APIC_TMICT, 0), Ok(1000));
        l.mmio_write(APIC_LVT_TIMER, 0x40, 100).unwrap();
        assert_eq!(l.next_timer_deadline(), Some(100 + 80_000));
    }

    #[test]
    fn masking_running_timer_cancels_it() {
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert!(l.next_timer_deadline().is_some());
        l.mmio_write(APIC_LVT_TIMER, 0x40 | LVT_MASK_BIT, 0)
            .unwrap();
        assert_eq!(l.next_timer_deadline(), None);
        assert_eq!(l.mmio_read(APIC_TMCCT, 1000), Ok(0));
    }

    #[test]
    fn enabling_apic_arms_loaded_timer() {
        let mut l = Lapic::new(LapicConfig {
            apic_id: 0,
            timer_hz: 25_000_000,
        })
        .unwrap();
        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert_eq!(l.next_timer_deadline(), None);
        l.mmio_write(APIC_SVR, 0xFF | SVR_ENABLE_BIT, 50).unwrap();
        assert_eq!(l.next_timer_deadline(), Some(50 + 80_000));
    }

    #[test]
    fn changing_timer_mode_to_tsc_deadline_cancels() {
        let mut l = enabled(25_000_000);
        l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
        l.mmio_write(APIC_TMICT, 1000, 0).unwrap();
        assert!(l.next_timer_deadline().is_some());
        l.mmio_write(APIC_LVT_TIMER, 0x40 | (0b10 << 17), 0)
            .unwrap();
        assert_eq!(l.next_timer_deadline(), None);
    }
}
