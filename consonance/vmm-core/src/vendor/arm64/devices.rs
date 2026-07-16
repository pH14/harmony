// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arm64 platform devices (pure logic, Mac-testable): the PL011 UART.
//!
//! The PL011 carries the 8250's *pattern* (`docs/ARCH-BOUNDARY.md` §B: "the
//! 8250 UART pattern itself carries"), not its registers: a serial-output
//! capture the engine's `SERL` hash chunk and the scrape stream read, an
//! injected-input queue for the task-81 `exec` verb (off-record, live-only —
//! never hashed, never snapshotted), and a small register-shadow file. The
//! GICv3 + generic-timer fabric is the `gicv3` crate's, not this module's.

use std::collections::VecDeque;

/// PL011 register offsets (Arm PrimeCell UART PL011 TRM). The subset the
/// skeleton models; in-range offsets outside it read as absent (`0`) and drop
/// writes (deny-ignore), mirroring the legacy-platform posture.
// Reached by guest MMIO only once the boot path composes the machine memory
// map (M3 wires `dispatch_mmio_arm64` → this model); until then only the
// capture/inject/restore seams are live.
#[allow(dead_code)]
pub(crate) mod reg {
    /// `UARTDR` — data register (write: transmit; read: receive).
    pub(crate) const DR: u64 = 0x000;
    /// `UARTFR` — flag register.
    pub(crate) const FR: u64 = 0x018;
    /// `UARTIBRD` — integer baud-rate divisor.
    pub(crate) const IBRD: u64 = 0x024;
    /// `UARTFBRD` — fractional baud-rate divisor.
    pub(crate) const FBRD: u64 = 0x028;
    /// `UARTLCR_H` — line control.
    pub(crate) const LCR_H: u64 = 0x02C;
    /// `UARTCR` — control register.
    pub(crate) const CR: u64 = 0x030;
    /// `UARTIMSC` — interrupt mask set/clear.
    pub(crate) const IMSC: u64 = 0x038;
    /// `UARTRIS` — raw interrupt status.
    pub(crate) const RIS: u64 = 0x03C;
    /// `UARTMIS` — masked interrupt status.
    pub(crate) const MIS: u64 = 0x040;
    /// `UARTICR` — interrupt clear (write-1-to-clear; no latched state in the
    /// skeleton, so a write is accepted and dropped).
    pub(crate) const ICR: u64 = 0x044;
    /// One past the last modeled byte (the PL011 occupies a 4 KiB page; the
    /// PrimeCell ID registers at `0xFE0..0x1000` read as absent).
    pub(crate) const SIZE: u64 = 0x1000;
}

/// `UARTFR.TXFE` — transmit FIFO empty (the model transmits instantly).
#[allow(dead_code)] // M3 wires the MMIO dispatch that reads FR
const FR_TXFE: u32 = 1 << 7;
/// `UARTFR.RXFE` — receive FIFO empty.
#[allow(dead_code)] // M3 wires the MMIO dispatch that reads FR
const FR_RXFE: u32 = 1 << 4;

/// The PL011 UART model: serial capture + `exec` input + register shadows.
#[derive(Clone, Debug, Default)]
pub(crate) struct Pl011 {
    /// Every byte the guest transmitted (`UARTDR` writes), in order — the
    /// guest-observable serial stream (`SERL` chunk, run result, scrape).
    capture: Vec<u8>,
    /// Injected serial input (task-81 `exec`): popped by guest `UARTDR` reads.
    /// **Off-record by ruling**: live-only, never hashed, never snapshotted,
    /// cleared on restore.
    rx: VecDeque<u8>,
    /// Shadows of the guest-programmed configuration registers, in
    /// [`Pl011::shadow_regs`] order: `IBRD`, `FBRD`, `LCR_H`, `CR`, `IMSC`.
    /// Residual state — it governs no skeleton behavior yet, but two runs that
    /// program the UART differently must hash differently.
    regs: [u32; 5],
}

impl Pl011 {
    /// A fresh (reset) PL011.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// `true` iff `offset` lies inside the PL011's 4 KiB register page.
    #[allow(dead_code)] // M3 wires the MMIO dispatch
    pub(crate) fn owns(offset: u64) -> bool {
        offset < reg::SIZE
    }

    /// Service a register load at `offset` (page-relative). Total: an
    /// unmodeled in-range offset reads as absent (`0`); the caller has already
    /// range-checked via [`Pl011::owns`].
    #[allow(dead_code)] // M3 wires the MMIO dispatch
    pub(crate) fn read(&mut self, offset: u64) -> u32 {
        match offset {
            // A DR read pops the next injected `exec` input byte, the way real
            // hardware pops the receive FIFO. Inert on every non-`exec` run.
            reg::DR => u32::from(self.rx.pop_front().unwrap_or(0)),
            reg::FR => {
                // Transmit never stalls (TXFE always set); receive-empty
                // reflects the injected-input queue.
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
            // No interrupt is ever raised by the skeleton model (delivery is
            // AA-6-gated), so both statuses read clear.
            reg::RIS | reg::MIS => 0,
            _ => 0,
        }
    }

    /// Service a register store at `offset` (page-relative). Total: a
    /// transmit lands in the capture, configuration writes land in shadows,
    /// everything else is accepted and dropped (deny-ignore).
    #[allow(dead_code)] // M3 wires the MMIO dispatch
    pub(crate) fn write(&mut self, offset: u64, value: u32) {
        match offset {
            reg::DR => self.capture.push(value as u8),
            reg::IBRD => self.regs[0] = value,
            reg::FBRD => self.regs[1] = value,
            reg::LCR_H => self.regs[2] = value,
            reg::CR => self.regs[3] = value,
            reg::IMSC => self.regs[4] = value,
            reg::ICR => {} // w1c with no latched state to clear
            _ => {}
        }
    }

    /// The serial output captured so far.
    pub(crate) fn capture(&self) -> &[u8] {
        &self.capture
    }

    /// Queue bytes on the guest's serial input (task-81 `exec`; off-record).
    pub(crate) fn inject_input(&mut self, bytes: &[u8]) {
        self.rx.extend(bytes.iter().copied());
    }

    /// The configuration-register shadows (`IBRD`, `FBRD`, `LCR_H`, `CR`,
    /// `IMSC`), for the device blob.
    pub(crate) fn shadow_regs(&self) -> &[u32; 5] {
        &self.regs
    }

    /// Overwrite the residual state from a snapshot. Clears the injected-input
    /// queue (input never survives a restore — off-record by ruling).
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
        u.write(0xFE0, 0xDEAD); // PrimeCell ID region: dropped
        assert_eq!(u.read(0xFE0), 0);
        assert_eq!(u.read(reg::RIS), 0);
        assert_eq!(u.read(reg::MIS), 0);
        assert!(Pl011::owns(0xFFF));
        assert!(!Pl011::owns(0x1000));
    }

    #[test]
    fn restore_overwrites_residuals_and_clears_input() {
        let mut u = Pl011::new();
        u.inject_input(b"stale");
        u.restore(b"prior".to_vec(), [1, 2, 3, 4, 5]);
        assert_eq!(u.capture(), b"prior");
        assert_eq!(u.shadow_regs(), &[1, 2, 3, 4, 5]);
        // Input never survives a restore.
        assert_eq!(u.read(reg::DR), 0);
    }
}
