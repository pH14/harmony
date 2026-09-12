// SPDX-License-Identifier: AGPL-3.0-or-later

pub const UART_PORT_BASE: u16 = 0x3F8;
pub const UART_PORT_LCR: u16 = 0x3FB;
pub const UART_PORT_LSR: u16 = 0x3FD;
pub const ISA_DEBUG_EXIT_PORT: u16 = 0x00F4;
pub const REPORT_PORT: u16 = 0x0CA2;
pub const UART_LCR_DLAB: u8 = 0x80;
pub const UART_LSR_THR_EMPTY: u8 = 0x60;

const UART_PORT_TOP: u16 = UART_PORT_BASE + 7;
const OFF_LSR: u16 = UART_PORT_LSR - UART_PORT_BASE;
const OFF_LCR: u16 = UART_PORT_LCR - UART_PORT_BASE;
const OFF_BASE: u16 = 0;
const OFF_IER: u16 = 1;
const OFF_IIR: u16 = 2;

const UART_LSR_DATA_READY: u8 = 0x01;
const UART_IER_RDI: u8 = 0x01;
const UART_IIR_RDI: u8 = 0x04;
const UART_IER_THRI: u8 = 0x02;
const UART_IIR_NONE: u8 = 0x01;
const UART_IIR_THRI: u8 = 0x02;

#[derive(Clone, Debug, Default)]
pub struct Uart8250 {
    capture: Vec<u8>,
    dlab: bool,
    regs: [u8; 8],
    dlm: u8,
    rx: std::collections::VecDeque<u8>,
}

impl Uart8250 {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn owns(port: u16) -> bool {
        (UART_PORT_BASE..=UART_PORT_TOP).contains(&port)
    }

    pub fn write(&mut self, port: u16, value: u8) -> bool {
        if !(UART_PORT_BASE..=UART_PORT_TOP).contains(&port) {
            return false;
        }
        let off = port - UART_PORT_BASE;
        match off {
            OFF_LCR => {
                self.dlab = value & UART_LCR_DLAB != 0;
                self.regs[OFF_LCR as usize] = value;
            }
            OFF_BASE if !self.dlab => {
                self.capture.push(value);
            }
            OFF_IER if self.dlab => {
                self.dlm = value;
            }
            _ => {
                self.regs[off as usize] = value;
            }
        }
        true
    }

    pub fn read(&self, port: u16) -> Option<u8> {
        if !(UART_PORT_BASE..=UART_PORT_TOP).contains(&port) {
            return None;
        }
        let off = port - UART_PORT_BASE;
        let value = match off {
            OFF_LSR => UART_LSR_THR_EMPTY | self.rx_status_bits(),
            OFF_IIR => self.iir_value(),
            OFF_BASE if !self.dlab => self.rx.front().copied().unwrap_or(0),
            OFF_IER if self.dlab => self.dlm,
            _ => self.regs[off as usize],
        };
        Some(value)
    }

    pub(crate) fn read_in(&mut self, port: u16) -> Option<u8> {
        if !(UART_PORT_BASE..=UART_PORT_TOP).contains(&port) {
            return None;
        }
        let off = port - UART_PORT_BASE;
        if off == OFF_BASE && !self.dlab {
            return Some(self.rx.pop_front().unwrap_or(0));
        }
        self.read(port)
    }

    pub(crate) fn inject_input(&mut self, bytes: &[u8]) {
        self.rx.extend(bytes.iter().copied());
    }

    pub(crate) fn rx_has_input(&self) -> bool {
        !self.rx.is_empty()
    }

    fn rx_status_bits(&self) -> u8 {
        if !self.dlab && self.rx_has_input() {
            UART_LSR_DATA_READY
        } else {
            0
        }
    }

    pub(crate) fn rx_irq_asserted(&self) -> bool {
        !self.dlab && self.rx_has_input() && (self.regs[OFF_IER as usize] & UART_IER_RDI != 0)
    }

    pub(crate) fn serial_irq_asserted(&self) -> bool {
        self.rx_irq_asserted() || self.thre_irq_asserted()
    }

    pub(crate) fn thre_irq_asserted(&self) -> bool {
        !self.dlab && (self.regs[OFF_IER as usize] & UART_IER_THRI != 0)
    }

    fn iir_value(&self) -> u8 {
        if self.rx_irq_asserted() {
            UART_IIR_RDI
        } else if self.thre_irq_asserted() {
            UART_IIR_THRI
        } else {
            UART_IIR_NONE
        }
    }

    pub fn capture(&self) -> &[u8] {
        &self.capture
    }

    pub fn shadow_regs(&self) -> &[u8; 8] {
        &self.regs
    }

    pub fn dlab(&self) -> bool {
        self.dlab
    }

    pub(crate) fn dlm(&self) -> u8 {
        self.dlm
    }

    pub(crate) fn restore(&mut self, capture: Vec<u8>, regs: [u8; 8], dlab: bool, dlm: u8) {
        self.capture = capture;
        self.regs = regs;
        self.dlab = dlab;
        self.dlm = dlm;
        self.rx.clear();
    }
}

#[derive(Clone, Debug)]
pub struct LegacyPlatform {
    config_address: u32,
    master_imr: u8,
    slave_imr: u8,
}

const PCI_CONFIG_ADDRESS: u16 = 0x0CF8;
const PCI_CONFIG_DATA_LO: u16 = 0x0CFC;
const PCI_CONFIG_DATA_HI: u16 = 0x0CFF;
const PIC_MASTER_DATA: u16 = 0x0021;
const PIC_SLAVE_DATA: u16 = 0x00A1;
const I8042_STATUS_PORT: u16 = 0x0064;
const I8042_STATUS_FAST_CLEAR: u8 = 0x01;

impl LegacyPlatform {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            config_address: 0,
            master_imr: 0xFF,
            slave_imr: 0xFF,
        }
    }

    pub fn owns(port: u16) -> bool {
        matches!(port,
            0x0020 | 0x0021 | 0x00A0 | 0x00A1
            | 0x0040..=0x0043
            | 0x0060 | 0x0064
            | 0x0061
            | 0x0070 | 0x0071
            | 0x0080..=0x008F
            | 0x04D0 | 0x04D1
            | PCI_CONFIG_ADDRESS..=0x0CFB
            | PCI_CONFIG_DATA_LO..=PCI_CONFIG_DATA_HI
            | 0x02F8..=0x02FF | 0x03E8..=0x03EF | 0x02E8..=0x02EF
        )
    }

    pub fn write(&mut self, port: u16, size: u8, value: u32) {
        if (PCI_CONFIG_ADDRESS..=0x0CFB).contains(&port) {
            if size == 4 && port == PCI_CONFIG_ADDRESS {
                self.config_address = value;
            }
        } else if port == PIC_MASTER_DATA {
            self.master_imr = value as u8;
        } else if port == PIC_SLAVE_DATA {
            self.slave_imr = value as u8;
        }
    }

    pub fn read(&self, port: u16, size: u8) -> u64 {
        let all_ones = match size {
            1 => 0x0000_00FF,
            2 => 0x0000_FFFF,
            _ => 0xFFFF_FFFF,
        };
        match port {
            PCI_CONFIG_ADDRESS..=0x0CFB => u64::from(self.config_address),
            PCI_CONFIG_DATA_LO..=PCI_CONFIG_DATA_HI => all_ones,
            PIC_MASTER_DATA => u64::from(self.master_imr),
            PIC_SLAVE_DATA => u64::from(self.slave_imr),
            0x02F8..=0x02FF | 0x03E8..=0x03EF | 0x02E8..=0x02EF => all_ones,
            I8042_STATUS_PORT => u64::from(I8042_STATUS_FAST_CLEAR),
            _ => 0,
        }
    }

    pub(crate) fn irq_masked(&self, irq: u8) -> bool {
        match irq {
            0..=7 => self.master_imr & (1 << irq) != 0,
            8..=15 => self.slave_imr & (1 << (irq - 8)) != 0,
            _ => true,
        }
    }

    pub fn config_address(&self) -> u32 {
        self.config_address
    }

    pub(crate) fn pic_imr(&self) -> [u8; 2] {
        [self.master_imr, self.slave_imr]
    }

    pub(crate) fn restore(&mut self, config_address: u32, master_imr: u8, slave_imr: u8) {
        self.config_address = config_address;
        self.master_imr = master_imr;
        self.slave_imr = slave_imr;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsr_reports_thr_empty() {
        let u = Uart8250::new();
        assert_eq!(u.read(UART_PORT_LSR), Some(UART_LSR_THR_EMPTY));
    }

    #[test]
    fn thr_writes_captured_in_order_dlab_clear() {
        let mut u = Uart8250::new();
        for &b in b"Hi" {
            assert!(u.write(UART_PORT_BASE, b));
        }
        assert_eq!(u.capture(), b"Hi");
    }

    #[test]
    fn init_order_does_not_capture_divisor() {
        let mut u = Uart8250::new();
        u.write(UART_PORT_BASE + 1, 0x00);
        u.write(UART_PORT_LCR, 0x80);
        u.write(UART_PORT_BASE, 0x01);
        u.write(UART_PORT_BASE + 1, 0x00);
        u.write(UART_PORT_LCR, 0x03);
        u.write(UART_PORT_BASE + 2, 0xC7);
        u.write(UART_PORT_BASE + 4, 0x03);
        for &b in b"PAYLOAD" {
            u.write(UART_PORT_BASE, b);
        }
        assert_eq!(u.capture(), b"PAYLOAD");
    }

    #[test]
    fn divisor_latch_read_back_with_dlab_set() {
        let mut u = Uart8250::new();
        u.write(UART_PORT_LCR, 0x80);
        u.write(UART_PORT_BASE, 0x01);
        assert_eq!(u.read(UART_PORT_BASE), Some(0x01));
        u.write(UART_PORT_LCR, 0x00);
        assert_eq!(u.read(UART_PORT_BASE), Some(0));
        assert!(u.capture().is_empty());
    }

    #[test]
    fn owns_is_the_com1_block_only() {
        assert!(Uart8250::owns(UART_PORT_BASE));
        assert!(Uart8250::owns(UART_PORT_BASE + 7));
        assert!(!Uart8250::owns(UART_PORT_BASE + 8));
        assert!(!Uart8250::owns(UART_PORT_BASE - 1));
    }

    #[test]
    fn shadow_regs_and_dlab_reflect_writes() {
        let mut u = Uart8250::new();
        assert!(!u.dlab(), "DLAB clear at reset");
        u.write(UART_PORT_BASE + 1, 0xAB);
        assert_eq!(u.shadow_regs()[1], 0xAB);
        u.write(UART_PORT_LCR, UART_LCR_DLAB);
        assert!(u.dlab());
        assert_eq!(u.shadow_regs()[3] & UART_LCR_DLAB, UART_LCR_DLAB);
        u.write(UART_PORT_LCR, 0x03);
        assert!(!u.dlab());
    }

    #[test]
    fn out_of_range_ports_not_owned() {
        let mut u = Uart8250::new();
        assert!(!u.write(0x3E0, 0xFF));
        assert_eq!(u.read(0x3E0), None);
        assert_eq!(u.read(ISA_DEBUG_EXIT_PORT), None);
    }

    #[test]
    fn iir_reports_thre_interrupt_only_when_thri_enabled() {
        let mut u = Uart8250::new();
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(0x01));
        assert!(!u.thre_irq_asserted());
        u.write(UART_PORT_BASE + 2, 0xC7);
        assert_eq!(
            u.read(UART_PORT_BASE + 2),
            Some(0x01),
            "IIR is the interrupt status, not the FCR shadow"
        );
        assert!(!u.thre_irq_asserted());
        u.write(UART_PORT_BASE + 1, UART_IER_THRI);
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(0x02));
        assert!(u.thre_irq_asserted());
        u.write(UART_PORT_BASE + 1, 0x01);
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(0x01));
        assert!(!u.thre_irq_asserted());
        u.write(UART_PORT_BASE + 1, UART_IER_THRI);
        assert!(u.thre_irq_asserted());
        u.write(UART_PORT_BASE + 1, 0x00);
        assert!(!u.thre_irq_asserted());
    }

    #[test]
    fn serial_input_is_inert_until_injected() {
        let u = Uart8250::new();
        assert!(!u.rx_has_input());
        assert_eq!(u.read(UART_PORT_LSR), Some(UART_LSR_THR_EMPTY), "no DR bit");
        assert_eq!(u.read(UART_PORT_BASE), Some(0), "RBR is 0 with no input");
        assert!(!u.rx_irq_asserted());
        assert!(!u.serial_irq_asserted());
    }

    #[test]
    fn injected_input_is_consumed_fifo_with_data_ready() {
        let mut u = Uart8250::new();
        u.inject_input(b"hi");
        assert!(u.rx_has_input());
        assert_eq!(
            u.read(UART_PORT_LSR),
            Some(UART_LSR_THR_EMPTY | UART_LSR_DATA_READY)
        );
        assert_eq!(u.read(UART_PORT_BASE), Some(b'h'));
        assert_eq!(u.read(UART_PORT_BASE), Some(b'h'), "peek does not consume");
        assert_eq!(u.read_in(UART_PORT_BASE), Some(b'h'));
        assert_eq!(u.read_in(UART_PORT_BASE), Some(b'i'));
        assert!(!u.rx_has_input());
        assert_eq!(u.read_in(UART_PORT_BASE), Some(0), "empty queue reads 0");
        assert_eq!(u.read(UART_PORT_LSR), Some(UART_LSR_THR_EMPTY));
    }

    #[test]
    fn receive_interrupt_asserts_only_when_enabled_and_queued() {
        let mut u = Uart8250::new();
        u.write(UART_PORT_BASE + 1, UART_IER_RDI | UART_IER_THRI);
        assert!(!u.rx_irq_asserted(), "no input ⇒ no receive IRQ");
        assert!(u.thre_irq_asserted());
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(UART_IIR_THRI));
        u.inject_input(b"x");
        assert!(u.rx_irq_asserted());
        assert!(u.serial_irq_asserted());
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(UART_IIR_RDI));
        assert_eq!(u.read_in(UART_PORT_BASE), Some(b'x'));
        assert!(!u.rx_irq_asserted());
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(UART_IIR_THRI));
        u.write(UART_PORT_BASE + 1, UART_IER_THRI);
        u.inject_input(b"y");
        assert!(!u.rx_irq_asserted(), "RDI disabled ⇒ no receive IRQ");
        assert_eq!(
            u.read(UART_PORT_LSR).unwrap() & UART_LSR_DATA_READY,
            UART_LSR_DATA_READY
        );
    }

    #[test]
    fn restore_clears_injected_input() {
        let mut u = Uart8250::new();
        u.inject_input(b"leftover");
        u.restore(vec![], [0; 8], false, 0);
        assert!(!u.rx_has_input());
        assert_eq!(u.read(UART_PORT_BASE), Some(0));
    }

    #[test]
    fn thri_ignored_while_dlab_selects_the_divisor_latch() {
        let mut u = Uart8250::new();
        u.write(UART_PORT_LCR, UART_LCR_DLAB);
        u.write(UART_PORT_BASE + 1, 0x02);
        assert!(
            !u.thre_irq_asserted(),
            "offset+1 is DLM here, not IER — no THRE assert"
        );
        assert_eq!(u.read(UART_PORT_BASE + 1), Some(0x02));
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(0x01));
        u.write(UART_PORT_LCR, 0x03);
        assert!(
            !u.thre_irq_asserted(),
            "DLM must not leak into IER: a divisor write is not a THRI enable"
        );
        assert_eq!(u.read(UART_PORT_BASE + 1), Some(0x00), "IER unset, reads 0");
        u.write(UART_PORT_BASE + 1, UART_IER_THRI);
        assert!(u.thre_irq_asserted());
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(0x02));
    }

    #[test]
    fn ier_preserved_across_divisor_latch_window() {
        let mut u = Uart8250::new();
        u.write(UART_PORT_BASE + 1, UART_IER_THRI);
        assert!(u.thre_irq_asserted());
        u.write(UART_PORT_LCR, UART_LCR_DLAB);
        u.write(UART_PORT_BASE, 0x03);
        u.write(UART_PORT_BASE + 1, 0x09);
        assert_eq!(u.read(UART_PORT_BASE + 1), Some(0x09), "DLM reads back");
        u.write(UART_PORT_LCR, 0x03);
        assert!(
            u.thre_irq_asserted(),
            "IER.THRI must survive a divisor-latch write"
        );
        assert_eq!(u.read(UART_PORT_BASE + 1), Some(UART_IER_THRI));
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(UART_IIR_THRI));
    }

    #[test]
    fn thr_capture_independent_of_thri() {
        let mut u = Uart8250::new();
        u.write(UART_PORT_BASE + 1, UART_IER_THRI);
        for &b in b"GUEST_READY" {
            assert!(u.write(UART_PORT_BASE, b));
        }
        assert_eq!(u.capture(), b"GUEST_READY");
    }

    #[test]
    fn legacy_owns_the_curated_ports_only() {
        for p in [
            0x0020, 0x0021, 0x00A0, 0x00A1, 0x0040, 0x0043, 0x0060, 0x0064, 0x0061, 0x0070, 0x0071,
            0x0080, 0x008F, 0x04D0, 0x04D1, 0x0CF8, 0x0CFB, 0x0CFC, 0x0CFF, 0x02F8, 0x03E8, 0x02E8,
        ] {
            assert!(LegacyPlatform::owns(p), "{p:#06x} should be owned");
        }
        for p in [0x03F8, 0x00F4, 0x0CA2, 0x0000, 0x1234, 0x0CF7, 0x0090] {
            assert!(!LegacyPlatform::owns(p), "{p:#06x} should not be owned");
        }
    }

    #[test]
    fn legacy_pci_config_address_round_trips_and_data_reads_no_device() {
        let mut p = LegacyPlatform::new();
        p.write(0x0CF8, 4, 0x8000_1000);
        assert_eq!(p.config_address(), 0x8000_1000);
        assert_eq!(p.read(0x0CF8, 4), 0x8000_1000);
        assert_eq!(p.read(0x0CFC, 4), 0xFFFF_FFFF);
        assert_eq!(p.read(0x0CFC, 2), 0x0000_FFFF);
        assert_eq!(p.read(0x0CFE, 1), 0x0000_00FF);
    }

    #[test]
    fn legacy_only_dword_write_to_cf8_latches() {
        let mut p = LegacyPlatform::new();
        p.write(0x0CFC, 4, 0xDEAD_BEEF);
        assert_eq!(
            p.config_address(),
            0,
            "dword write to non-CF8 must not latch"
        );
        p.write(0x0CF8, 1, 0xDEAD_BEEF);
        assert_eq!(p.config_address(), 0, "byte write to CF8 must not latch");
        p.write(0x0CF8, 2, 0xDEAD_BEEF);
        assert_eq!(p.config_address(), 0, "word write to CF8 must not latch");
        p.write(0x0CF8, 4, 0xCAFE_F00D);
        assert_eq!(p.config_address(), 0xCAFE_F00D);
    }

    #[test]
    fn legacy_reads_give_absent_idle_values() {
        let p = LegacyPlatform::new();
        assert_eq!(p.read(0x0021, 1), 0xFF);
        assert_eq!(p.read(0x00A1, 1), 0xFF);
        assert_eq!(p.read(0x02F8, 1), 0xFF);
        for port in [0x0040, 0x0071, 0x0080, 0x04D0, 0x0020, 0x0061, 0x0060] {
            assert_eq!(p.read(port, 1), 0, "{port:#06x} should read idle");
        }
        let mut p = p;
        p.write(0x0043, 1, 0x36);
        assert_eq!(
            p.config_address(),
            0,
            "non-PCI writes leave the latch alone"
        );
        assert_eq!(
            p.pic_imr(),
            [0xFF, 0xFF],
            "non-PIC writes leave the IMRs alone"
        );
    }

    #[test]
    fn i8042_status_reports_obf_set_so_the_probe_fails_fast() {
        let p = LegacyPlatform::new();
        let status = p.read(0x0064, 1);
        assert_eq!(status, 0x01, "0x64 status must read OBF-set, IBF-clear");
        assert_eq!(status & 0x01, 0x01, "OBF (bit 0) must be set");
        assert_eq!(status & 0x02, 0x00, "IBF (bit 1) must be clear");
        assert_eq!(p.read(0x0060, 1), 0, "0x60 data port stays idle");
        let mut p = p;
        p.write(0x0064, 1, 0xFF);
        assert_eq!(p.read(0x0064, 1), 0x01, "status is stateless / constant");
    }

    #[test]
    fn pic_imr_latches_so_probe_8259a_sees_a_real_pic() {
        let mut p = LegacyPlatform::new();
        p.write(0x00A1, 1, 0xFF);
        p.write(0x0021, 1, 0xFB);
        assert_eq!(
            p.read(0x0021, 1),
            0xFB,
            "master IMR must read back verbatim"
        );
        assert_eq!(p.read(0x00A1, 1), 0xFF, "slave IMR must read back verbatim");
        assert_eq!(p.pic_imr(), [0xFB, 0xFF]);
        p.write(0x0021, 1, 0x12);
        assert_eq!(p.pic_imr(), [0x12, 0xFF]);
        p.write(0x00A1, 1, 0x34);
        assert_eq!(p.pic_imr(), [0x12, 0x34]);
    }

    #[test]
    fn irq_masked_reads_the_right_imr_bit() {
        let mut p = LegacyPlatform::new();
        assert!(p.irq_masked(4), "IRQ 4 masked at reset");
        assert!(p.irq_masked(0));
        assert!(p.irq_masked(8));
        p.write(0x0021, 1, 0xFF & !(1 << 4));
        assert!(!p.irq_masked(4), "IRQ 4 now unmasked");
        assert!(
            p.irq_masked(3),
            "IRQ 3 still masked (kills off-by-one shift)"
        );
        assert!(p.irq_masked(5), "IRQ 5 still masked");
        assert!(p.irq_masked(0), "master line 0 untouched");
        p.write(0x00A1, 1, 0xFF & !(1 << 0));
        assert!(!p.irq_masked(8), "IRQ 8 = slave bit 0 unmasked");
        assert!(p.irq_masked(9), "IRQ 9 = slave bit 1 still masked");
        assert!(
            !p.irq_masked(4),
            "the master IMR is untouched by a slave write"
        );
        assert!(
            p.irq_masked(3),
            "unmasking a slave line leaves master bit 3 masked"
        );
        assert!(p.irq_masked(16));
    }
}
