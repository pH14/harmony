// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::VecDeque;

pub(crate) mod reg {
    pub(crate) const DR: u64 = 0x000;
    pub(crate) const FR: u64 = 0x018;
    pub(crate) const IBRD: u64 = 0x024;
    pub(crate) const FBRD: u64 = 0x028;
    pub(crate) const LCR_H: u64 = 0x02C;
    pub(crate) const CR: u64 = 0x030;
    pub(crate) const IMSC: u64 = 0x038;
    pub(crate) const RIS: u64 = 0x03C;
    pub(crate) const MIS: u64 = 0x040;
    pub(crate) const ICR: u64 = 0x044;
}

const FR_TXFE: u32 = 1 << 7;
const FR_RXFE: u32 = 1 << 4;

#[derive(Clone, Debug, Default)]
pub(crate) struct Pl011 {
    capture: Vec<u8>,
    rx: VecDeque<u8>,
    regs: [u32; 5],
}

impl Pl011 {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn read(&mut self, offset: u64) -> u32 {
        match offset {
            reg::DR => u32::from(self.rx.pop_front().unwrap_or(0)),
            reg::FR => {
                let mut fr = FR_TXFE;
                if self.rx.is_empty() {
                    fr |= FR_RXFE;
                }
                fr
            }
            reg::IBRD => self.regs[0],
            reg::FBRD => self.regs[1],
            reg::LCR_H => self.regs[2],
            reg::CR => self.regs[3],
            reg::IMSC => self.regs[4],
            reg::RIS | reg::MIS => 0,
            _ => 0,
        }
    }

    pub(crate) fn write(&mut self, offset: u64, value: u32) {
        match offset {
            reg::DR => self.capture.push(value as u8),
            reg::IBRD => self.regs[0] = value,
            reg::FBRD => self.regs[1] = value,
            reg::LCR_H => self.regs[2] = value,
            reg::CR => self.regs[3] = value,
            reg::IMSC => self.regs[4] = value,
            reg::ICR => {}
            _ => {}
        }
    }

    pub(crate) fn capture(&self) -> &[u8] {
        &self.capture
    }

    pub(crate) fn inject_input(&mut self, bytes: &[u8]) {
        self.rx.extend(bytes.iter().copied());
    }

    pub(crate) fn shadow_regs(&self) -> &[u32; 5] {
        &self.regs
    }

    pub(crate) fn restore(&mut self, capture: Vec<u8>, regs: [u32; 5]) {
        self.capture = capture;
        self.regs = regs;
        self.rx.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dr_writes_are_captured_in_order() {
        let mut u = Pl011::new();
        for b in b"HARMONY" {
            u.write(reg::DR, u32::from(*b));
        }
        assert_eq!(u.capture(), b"HARMONY");
    }

    #[test]
    fn fr_reports_tx_empty_and_rx_state() {
        let mut u = Pl011::new();
        assert_eq!(u.read(reg::FR), 0x90, "PL011 FR: TXFE is bit 7, RXFE bit 4");
        assert_eq!(u.read(reg::FR), FR_TXFE | FR_RXFE);
        u.inject_input(b"x");
        assert_eq!(u.read(reg::FR), FR_TXFE, "input queued clears RXFE");
        assert_eq!(u.read(reg::DR), u32::from(b'x'));
        assert_eq!(
            u.read(reg::FR),
            FR_TXFE | FR_RXFE,
            "drained queue re-sets RXFE"
        );
    }

    #[test]
    fn config_writes_shadow_and_read_back() {
        let mut u = Pl011::new();
        u.write(reg::IBRD, 13);
        u.write(reg::FBRD, 1);
        u.write(reg::LCR_H, 0x70);
        u.write(reg::CR, 0x301);
        u.write(reg::IMSC, 0x10);
        assert_eq!(u.shadow_regs(), &[13, 1, 0x70, 0x301, 0x10]);
        assert_eq!(u.read(reg::IBRD), 13);
        assert_eq!(u.read(reg::CR), 0x301);
    }

    #[test]
    fn unmodeled_offsets_read_absent_and_drop_writes() {
        let mut u = Pl011::new();
        u.write(0xFE0, 0xDEAD);
        assert_eq!(u.read(0xFE0), 0);
        assert_eq!(u.read(reg::RIS), 0);
        assert_eq!(u.read(reg::MIS), 0);
    }

    #[test]
    fn restore_overwrites_residuals_and_clears_input() {
        let mut u = Pl011::new();
        u.inject_input(b"stale");
        u.restore(b"prior".to_vec(), [1, 2, 3, 4, 5]);
        assert_eq!(u.capture(), b"prior");
        assert_eq!(u.shadow_regs(), &[1, 2, 3, 4, 5]);
        assert_eq!(u.read(reg::DR), 0);
    }
}
