// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bring-up device shims (pure logic, Mac-testable): a minimal polled 8250 UART
//! and the isa-debug-exit port constant.
//!
//! The UART models exactly enough of COM1 for the task-04 payloads' polled-write
//! console: it accepts the init writes (IER/FCR/LCR/MCR/divisor) without modeling
//! baud, reports THR-empty on every LSR read so the guest's spin-loop always makes
//! progress, and — critically — **tracks `LCR.DLAB`** so the `0x01` divisor byte
//! the init writes to `0x3F8` is captured as a divisor latch, **not** prepended to
//! the serial output (which would fail the M1 golden byte-for-byte).

/// 8250 base: THR/RBR when `LCR.DLAB=0`, DLL when `DLAB=1`.
pub const UART_PORT_BASE: u16 = 0x3F8;
/// Line control register (bit 7 = DLAB).
pub const UART_PORT_LCR: u16 = 0x3FB;
/// Line status register.
pub const UART_PORT_LSR: u16 = 0x3FD;
/// isa-debug-exit port — a `u8` write terminates the run (0 = PASS, 1 = FAIL).
pub const ISA_DEBUG_EXIT_PORT: u16 = 0x00F4;
/// Conformance **report channel** port (corpus box-integration). Each
/// `OUT REPORT_PORT, EAX` (a 32-bit write) appends `EAX` to the VM's ordered
/// report stream (`report(u64)` is two writes: low dword then high); the host
/// captures the stream for the O2 conformance oracle. Distinct from #44's
/// hypercall doorbell at `0x0CA1` (adjacent, but its own dedicated port so a
/// reported value can never be mistaken for a doorbell ring). Documented in
/// `docs/INTEGRATION.md` ("report channel") and `docs/cpu-msr-contract.toml`
/// `[ports]`; it carries no per-host input, so it is **not** a §6-hashed row.
pub const REPORT_PORT: u16 = 0x0CA2;
/// `LCR.DLAB` (bit 7). When set, `UART_PORT_BASE` (+1) address the divisor latch
/// (DLL/DLM), **not** THR/RBR/IER. The model must track it.
pub const UART_LCR_DLAB: u8 = 0x80;
/// LSR value reported on read: THR-empty + transmitter-empty (bits 5 and 6) so
/// the guest's polled-write loop always makes progress. No data-ready bit (we
/// never feed input).
pub const UART_LSR_THR_EMPTY: u8 = 0x60;

/// Highest port the COM1 register block occupies (`UART_PORT_BASE + 7`).
const UART_PORT_TOP: u16 = UART_PORT_BASE + 7;
/// Register-block offset of the line status register (`0x3FD - 0x3F8`).
const OFF_LSR: u16 = UART_PORT_LSR - UART_PORT_BASE;
/// Register-block offset of the line control register (`0x3FB - 0x3F8`).
const OFF_LCR: u16 = UART_PORT_LCR - UART_PORT_BASE;
/// Register-block offset of THR/RBR/DLL (`0`).
const OFF_BASE: u16 = 0;
/// Register-block offset of IER (`+1`, when `LCR.DLAB == 0`; the divisor-latch
/// high byte when `DLAB == 1`).
const OFF_IER: u16 = 1;
/// Register-block offset of IIR (read) / FCR (write) (`+2`). IIR is read-only and
/// reports the **interrupt status**; FCR is write-only and selects the FIFO —
/// they share the port but are distinct registers, so the model computes IIR on
/// read rather than echoing the shadowed FCR byte.
const OFF_IIR: u16 = 2;

/// `IER` bit 1 — **THRE interrupt enable** (transmitter-holding-register empty).
/// When the guest sets it (with `DLAB` clear) the kernel's interrupt-driven 8250
/// TX path expects the COM1 line (IRQ 4) to fire as soon as THR is empty; that is
/// the interrupt this model raises (see [`Uart8250::thre_irq_asserted`]).
const UART_IER_THRI: u8 = 0x02;
/// `IIR` value reported when **no** interrupt is pending: bit 0 (`NO_INT`) set,
/// FIFO bits clear. A `16450`-style read (`iir >> 6 == 0`) so the kernel's
/// autoconfig keeps treating COM1 as a non-FIFO part (matches the live boot's
/// `is a 16450`).
const UART_IIR_NONE: u8 = 0x01;
/// `IIR` value reported when the **THRE** (transmitter-empty) interrupt is
/// pending: bit 0 (`NO_INT`) clear, interrupt-id `0b001`. The kernel's
/// THRE/TXEN-bug probes read this to conclude the UART interrupt works (so it
/// uses the IRQ path), and its IRQ handler reads it to dispatch `tx_chars`.
const UART_IIR_THRI: u8 = 0x02;

/// Minimal 8250: accepts init writes (IER/FCR/LCR/MCR/divisor) without modeling
/// baud; LSR reads return [`UART_LSR_THR_EMPTY`]. It **tracks `LCR.DLAB`**: a
/// write to [`UART_PORT_BASE`] is appended to [`Self::capture`] **only when DLAB
/// is clear** (a real THR transmit). With DLAB set, that port is the
/// divisor-latch-low byte — shadowed, not captured — so task-04's `0x01` baud
/// divisor never becomes a stray `\x01` in the serial output. Pure; no I/O.
///
/// **Interrupt-driven TX (the Linux userspace console path).** The kernel's tty
/// write path enables the THRE interrupt (`IER` bit 1) and drains the TX buffer
/// from the COM1 IRQ-4 handler, not by polling. So the model reports the THRE
/// interrupt: a read of `IIR` (offset 2) returns [`UART_IIR_THRI`] when `IER.THRI`
/// is set and THR is empty (always, here — TX drains instantly to [`Self::capture`])
/// and [`UART_IIR_NONE`] otherwise, and [`Self::thre_irq_asserted`] exposes that
/// same condition as the COM1 interrupt line for the VMM to route to IRQ 4. (The
/// polled M1/M2 payloads never touch `IIR` and keep `IER` zeroed, so this is inert
/// for them — they only ever read `LSR`.)
#[derive(Clone, Debug, Default)]
pub struct Uart8250 {
    /// THR transmit capture buffer (DLAB-clear `0x3F8` writes), in order.
    capture: Vec<u8>,
    /// `true` when `LCR.DLAB` is set (the divisor-latch window is active).
    dlab: bool,
    /// Benign register shadows for offsets 0..=7. Offset 1 is the **IER** — the
    /// divisor-latch-high (DLM) byte is held separately in [`Self::dlm`] so a
    /// DLAB-window write never clobbers it. Read back so the guest's init reads are
    /// consistent.
    regs: [u8; 8],
    /// Divisor-latch **high** byte (DLM), the offset-1 register **when `DLAB` is
    /// set**. Kept distinct from the IER shadow (`regs[1]`, the offset-1 register
    /// when `DLAB` is clear): writing the divisor must not overwrite the IER, or a
    /// later [`Self::thre_irq_asserted`] would read the divisor byte as if it were
    /// the IER. Not folded into the state hash — it drives no model logic and is 0
    /// on the polled M1/M2/corpus paths (divisor `0x0001`), so omitting it keeps
    /// those `DEV`-chunk hashes byte-identical.
    dlm: u8,
}

impl Uart8250 {
    /// A fresh, reset UART: empty capture, DLAB clear, zeroed shadows.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `port` is in the COM1 register block this model owns
    /// (`UART_PORT_BASE ..= UART_PORT_BASE + 7`, i.e. `0x3F8..=0x3FF`). The event
    /// loop calls this to gate the byte-width check (an 8250 register is
    /// byte-addressed) **before** servicing an access, so a non-byte access to a
    /// modeled port fails closed instead of being truncated.
    pub fn owns(port: u16) -> bool {
        (UART_PORT_BASE..=UART_PORT_TOP).contains(&port)
    }

    /// Service a guest port write (incl. `LCR`, which updates DLAB). Returns
    /// whether the port belonged to this device. A [`UART_PORT_BASE`] write
    /// appends to [`Self::capture`] **only if `LCR.DLAB == 0`**; with DLAB set it
    /// is the divisor latch — shadowed, not captured.
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
                // THR transmit — the only bytes that are serial output.
                self.capture.push(value);
            }
            OFF_IER if self.dlab => {
                // DLAB set ⇒ offset 1 is the divisor-latch high byte (DLM), **not**
                // the IER. Shadow it separately so the IER (`regs[1]`) survives the
                // divisor-programming window intact.
                self.dlm = value;
            }
            _ => {
                // Divisor latch low (DLAB set), IER/FCR/MCR, or any other benign
                // init register: shadow it, never capture.
                self.regs[off as usize] = value;
            }
        }
        true
    }

    /// Service a guest port read; `Some(byte)` if this device owns `port`, else
    /// `None`. LSR → [`UART_LSR_THR_EMPTY`]; IIR → the computed interrupt status
    /// ([`UART_IIR_THRI`] when the THRE interrupt is asserted, else
    /// [`UART_IIR_NONE`]); [`UART_PORT_BASE`] with DLAB set → the divisor-latch
    /// shadow, with DLAB clear → `0` (RBR; we never feed input).
    pub fn read(&self, port: u16) -> Option<u8> {
        if !(UART_PORT_BASE..=UART_PORT_TOP).contains(&port) {
            return None;
        }
        let off = port - UART_PORT_BASE;
        let value = match off {
            OFF_LSR => UART_LSR_THR_EMPTY,
            // IIR is a distinct read-only register from the FCR written at the same
            // port: report the live interrupt status, not the FCR shadow (a stale
            // `NO_INT`-clear FCR byte would make the kernel believe an interrupt is
            // always pending and mis-detect the TX path).
            OFF_IIR => self.iir_value(),
            OFF_BASE if !self.dlab => 0, // RBR with no pending input
            // DLAB set ⇒ offset 1 reads back the divisor-latch high byte (DLM), the
            // companion of the offset-1 write split above — never the IER shadow.
            OFF_IER if self.dlab => self.dlm,
            _ => self.regs[off as usize],
        };
        Some(value)
    }

    /// Is the COM1 **THRE (transmitter-empty) interrupt** currently asserted? True
    /// iff the guest has enabled `IER.THRI` (with `DLAB` clear, so the offset-1
    /// shadow is the IER and not the divisor-latch high byte) — THR is modeled as
    /// always empty (TX drains instantly into [`Self::capture`]), so an enabled
    /// THRE interrupt is always pending. This is the COM1 IRQ-4 line the VMM routes
    /// to the guest; it is **edge-driven by the guest's own `IER` write**, so its
    /// timing is a deterministic function of guest execution (no V-time, no
    /// wall-clock).
    pub(crate) fn thre_irq_asserted(&self) -> bool {
        !self.dlab && (self.regs[OFF_IER as usize] & UART_IER_THRI != 0)
    }

    /// The value a read of the IIR (offset 2) returns: [`UART_IIR_THRI`] while the
    /// THRE interrupt is asserted, else [`UART_IIR_NONE`]. (No receive/line-status
    /// interrupts: the model never feeds input, so only THRE can be pending.)
    fn iir_value(&self) -> u8 {
        if self.thre_irq_asserted() {
            UART_IIR_THRI
        } else {
            UART_IIR_NONE
        }
    }

    /// The bytes written to THR (DLAB clear) so far — the serial capture buffer,
    /// in order.
    pub fn capture(&self) -> &[u8] {
        &self.capture
    }

    /// The benign register shadows (offsets 0..=7: divisor latch, IER, FCR/IIR,
    /// LCR, MCR, MSR, SCR). Folded into the M2 state hash (`Vmm::state_blob`'s
    /// `DEV` chunk) so two runs that leave the UART in different register state —
    /// e.g. a different baud divisor or IER — hash differently even when the
    /// captured serial bytes are identical. (`capture()` is hashed separately.)
    pub fn shadow_regs(&self) -> &[u8; 8] {
        &self.regs
    }

    /// The latched `LCR.DLAB` window state, also folded into the state hash: two
    /// runs that end with DLAB set vs clear differ in future port-I/O behavior
    /// (port `0x3F8` addresses the divisor latch vs THR/RBR), so they must hash
    /// differently.
    pub fn dlab(&self) -> bool {
        self.dlab
    }
}

/// Minimal **legacy PC platform** I/O for the Linux boot path: enough of the
/// classic ISA/PCI port space that the kernel's early probing finds **no devices**
/// (except a functional 8259 PIC) and moves on, rather than tripping the
/// default-deny `ContractViolation`. It is wired **only** on the Linux path
/// (alongside the xAPIC); M1/M2/corpus payloads never touch these ports, so it
/// does not exist for them.
///
/// Most of it is a stub: writes are accepted and dropped; reads return the
/// architectural "absent / idle" value (PCI config-data ⇒ all-ones = no device;
/// PIT/CMOS/POST ⇒ 0; an unpopulated COM port ⇒ all-ones so the 8250 autoconfig's
/// scratch test fails and the port is skipped).
///
/// The one non-idle read is the **i8042 keyboard-controller status** (`0x64`),
/// which reports OBF-set ([`I8042_STATUS_FAST_CLEAR`]) so the kernel's controller
/// probe fails fast ("No controller found") rather than spinning a 10000-iteration
/// `udelay` wait-for-OBF — a wait that clears in 0.33 s on stock KVM but strands
/// the **patched** boot for minutes (every `RDTSC` in the delay loop traps to
/// V-time). The guest has no keyboard/mouse, so aborting the probe loses nothing.
///
/// The **8259 PIC interrupt-mask registers** (`0x21` master, `0xA1` slave) are
/// **modeled as read/write latches**, not stubbed to all-ones. This is
/// load-bearing for interrupt delivery: the kernel's `probe_8259A` writes a known
/// value to `0x21` and reads it back — an all-ones read makes it decide there is
/// **no PIC** ("Using NULL legacy PIC"), which leaves `nr_legacy_irqs() == 0` so
/// the legacy IRQ lines (incl. COM1's IRQ 4) get **no interrupt controller** and
/// `request_irq(4)` fails — which is why the userspace console open/write fails.
/// Latching the IMR makes the probe pass, the kernel installs the real 8259, IRQ 4
/// gets a chip, and the VMM can deliver the serial interrupt. The IMR also gates
/// that delivery (a masked line is not injected). Retained state — the PCI
/// `CONFIG_ADDRESS` latch and both IMRs — is a pure function of guest execution,
/// folded into the state hash.
#[derive(Clone, Debug)]
pub struct LegacyPlatform {
    /// The PCI mechanism-1 `CONFIG_ADDRESS` (`0xCF8`) latch.
    config_address: u32,
    /// 8259 **master** PIC interrupt-mask register (port `0x21`); a set bit masks
    /// that IRQ line (IRQ 0..=7). Reset all-masked (`0xFF`).
    master_imr: u8,
    /// 8259 **slave** PIC interrupt-mask register (port `0xA1`); IRQ 8..=15.
    slave_imr: u8,
}

/// PCI `CONFIG_ADDRESS` port (mechanism 1), a 4-byte latch.
const PCI_CONFIG_ADDRESS: u16 = 0x0CF8;
/// PCI `CONFIG_DATA` window (`0xCFC..=0xCFF`); a byte/word/dword access reads the
/// selected register — all-ones here (no device populated).
const PCI_CONFIG_DATA_LO: u16 = 0x0CFC;
const PCI_CONFIG_DATA_HI: u16 = 0x0CFF;
/// 8259 master PIC data port — the interrupt-mask register (IMR), a read/write
/// latch the kernel probes ([`LegacyPlatform`] doc).
const PIC_MASTER_DATA: u16 = 0x0021;
/// 8259 slave PIC data port — the slave IMR.
const PIC_SLAVE_DATA: u16 = 0x00A1;
/// i8042 keyboard-controller **status** port (`0x64` read). The status byte we
/// return ([`I8042_STATUS_FAST_CLEAR`]) makes the kernel's controller-presence
/// check fail fast instead of spinning a jiffies timeout under patched V-time
/// ([`LegacyPlatform`] doc).
const I8042_STATUS_PORT: u16 = 0x0064;
/// The i8042 status byte returned on every `0x64` read: **OBF set** (bit 0,
/// output-buffer-full) with **IBF clear** (bit 1, input-buffer-full).
///
/// "OBF set, always" makes the kernel's `i8042_controller_check` → `i8042_flush`
/// drain its bounded `I8042_BUFFER_SIZE` (16) slots and then report **"No
/// controller found"** (`-ENODEV`) — so the i8042 driver aborts *before* it
/// creates the platform device and runs `i8042_controller_init`'s read-CTR
/// command. That read-CTR is the spin: it calls `i8042_wait_read`, which loops
/// `I8042_CTL_TIMEOUT` (10000) × `udelay(50)` waiting for OBF when our model never
/// sets it. On stock KVM that timeout clears in ~0.33 s, but on the **patched**
/// backend every `RDTSC` in the `delay_tsc` loop traps to V-time, so the same
/// 10000-iteration wait strands the boot for minutes. Reporting OBF-set caps the
/// i8042 cost at the 16-slot flush (IBF clear keeps any `i8042_wait_write` instant
/// too), and the guest needs no keyboard/mouse, so "no controller" is the honest
/// outcome. A constant — no state, so nothing to fold into the state hash.
const I8042_STATUS_FAST_CLEAR: u8 = 0x01;

impl LegacyPlatform {
    // No `Default` impl: the architectural reset (IMRs all-masked, `0xFF`) is not
    // the zero value a derived `Default` would give (all-*unmasked*), and a manual
    // `Default` would widen the frozen public API. Construct via `new`.
    #[allow(clippy::new_without_default)]
    /// A fresh platform: PCI address latch cleared, both PIC IMRs all-masked.
    ///
    /// The IMRs reset to "all masked" (`0xFF`, the quiescent state): no line
    /// delivers until the guest's 8259 init + per-IRQ unmask clears its bit, so
    /// nothing is injected before the kernel sets the controller up.
    pub fn new() -> Self {
        Self {
            config_address: 0,
            master_imr: 0xFF,
            slave_imr: 0xFF,
        }
    }

    /// Whether `port` is one of the curated legacy ports this stub services.
    pub fn owns(port: u16) -> bool {
        matches!(port,
            0x0020 | 0x0021 | 0x00A0 | 0x00A1            // 8259 PIC (master/slave)
            | 0x0040..=0x0043                            // 8254 PIT
            | 0x0060 | 0x0064                            // i8042 keyboard controller (data/status)
            | 0x0061                                     // NMI status / port B
            | 0x0070 | 0x0071                            // CMOS/RTC index+data
            | 0x0080..=0x008F                            // POST code + DMA page regs
            | 0x04D0 | 0x04D1                            // ELCR (PIC edge/level)
            | PCI_CONFIG_ADDRESS..=0x0CFB                 // PCI CONFIG_ADDRESS (4 bytes)
            | PCI_CONFIG_DATA_LO..=PCI_CONFIG_DATA_HI     // PCI CONFIG_DATA
            | 0x02F8..=0x02FF | 0x03E8..=0x03EF | 0x02E8..=0x02EF // COM2/COM4/COM3
        )
    }

    /// Service a write: latch the PCI `CONFIG_ADDRESS` and the 8259 IMRs, drop
    /// everything else.
    pub fn write(&mut self, port: u16, size: u8, value: u32) {
        if (PCI_CONFIG_ADDRESS..=0x0CFB).contains(&port) {
            // Mechanism-1 address register; a dword write replaces it. (Sub-dword
            // writes are rare and harmless to ignore — no device is populated.)
            if size == 4 && port == PCI_CONFIG_ADDRESS {
                self.config_address = value;
            }
        } else if port == PIC_MASTER_DATA {
            // 8259 master IMR (probed + per-IRQ (un)masked by the kernel).
            self.master_imr = value as u8;
        } else if port == PIC_SLAVE_DATA {
            self.slave_imr = value as u8;
        }
        // All other ports (incl. the PIC command ports 0x20/0xA0 and their EOIs):
        // accepted and dropped (no-op).
    }

    /// Service a read: return the architectural absent/idle value for `port`, or
    /// the live IMR latch for the 8259 data ports.
    pub fn read(&self, port: u16, size: u8) -> u64 {
        let all_ones = match size {
            1 => 0x0000_00FF,
            2 => 0x0000_FFFF,
            _ => 0xFFFF_FFFF,
        };
        match port {
            PCI_CONFIG_ADDRESS..=0x0CFB => u64::from(self.config_address),
            PCI_CONFIG_DATA_LO..=PCI_CONFIG_DATA_HI => all_ones, // no PCI device
            // PIC data ports read back the latched IMR — so `probe_8259A`'s
            // write-then-read sees the real PIC (not all-ones ⇒ "NULL legacy PIC").
            PIC_MASTER_DATA => u64::from(self.master_imr),
            PIC_SLAVE_DATA => u64::from(self.slave_imr),
            // An unpopulated COM port reads all-ones so the 8250 scratch test fails
            // and it is skipped.
            0x02F8..=0x02FF | 0x03E8..=0x03EF | 0x02E8..=0x02EF => all_ones,
            // i8042 status (0x64): OBF-set so the controller-presence check fails
            // fast ("No controller found") instead of spinning a 10000×udelay
            // wait-for-OBF under patched V-time. See `I8042_STATUS_FAST_CLEAR`.
            I8042_STATUS_PORT => u64::from(I8042_STATUS_FAST_CLEAR),
            // PIT, CMOS, POST, ELCR, PIC command, port B, i8042 data (0x60): idle.
            _ => 0,
        }
    }

    /// Whether the 8259 has IRQ `irq` (0..=15) masked in its IMR — master for
    /// 0..=7, slave for 8..=15. An out-of-range line is treated as masked (no
    /// delivery). The VMM gates serial-IRQ injection on this so a line the kernel
    /// masked (e.g. while its handler runs) is not re-injected.
    pub(crate) fn irq_masked(&self, irq: u8) -> bool {
        match irq {
            0..=7 => self.master_imr & (1 << irq) != 0,
            8..=15 => self.slave_imr & (1 << (irq - 8)) != 0,
            _ => true,
        }
    }

    /// The PCI `CONFIG_ADDRESS` latch — folded into the Linux-path state hash so a
    /// divergence in PCI probing is observable.
    pub fn config_address(&self) -> u32 {
        self.config_address
    }

    /// The 8259 master/slave IMR latches `[master, slave]` — folded into the
    /// Linux-path state hash alongside [`Self::config_address`], so two runs that
    /// leave the PIC masking different (hence future interrupt delivery different)
    /// hash differently.
    pub(crate) fn pic_imr(&self) -> [u8; 2] {
        [self.master_imr, self.slave_imr]
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
    fn task04_init_order_does_not_capture_divisor() {
        // Replay uart::init(): IER=0, LCR DLAB=1, divisor 0x01 to 0x3F8, DLM=0,
        // LCR 8N1 (DLAB=0), FCR, MCR — then the data bytes.
        let mut u = Uart8250::new();
        u.write(UART_PORT_BASE + 1, 0x00); // IER
        u.write(UART_PORT_LCR, 0x80); // DLAB=1
        u.write(UART_PORT_BASE, 0x01); // divisor low — MUST NOT be captured
        u.write(UART_PORT_BASE + 1, 0x00); // divisor high
        u.write(UART_PORT_LCR, 0x03); // 8N1, DLAB=0
        u.write(UART_PORT_BASE + 2, 0xC7); // FCR
        u.write(UART_PORT_BASE + 4, 0x03); // MCR
        for &b in b"PAYLOAD" {
            u.write(UART_PORT_BASE, b);
        }
        // No leading \x01: only the data bytes are captured.
        assert_eq!(u.capture(), b"PAYLOAD");
    }

    #[test]
    fn divisor_latch_read_back_with_dlab_set() {
        let mut u = Uart8250::new();
        u.write(UART_PORT_LCR, 0x80); // DLAB=1
        u.write(UART_PORT_BASE, 0x01); // DLL = 1
        assert_eq!(u.read(UART_PORT_BASE), Some(0x01));
        // With DLAB clear, the same port is RBR (no input → 0), capture untouched.
        u.write(UART_PORT_LCR, 0x00);
        assert_eq!(u.read(UART_PORT_BASE), Some(0));
        assert!(u.capture().is_empty());
    }

    #[test]
    fn owns_is_the_com1_block_only() {
        assert!(Uart8250::owns(UART_PORT_BASE)); // 0x3F8
        assert!(Uart8250::owns(UART_PORT_BASE + 7)); // 0x3FF, top of the block
        // Just past the block: kills the `UART_PORT_BASE + 7` → `* 7` mutant
        // (which would stretch the range to 0x3F8*7).
        assert!(!Uart8250::owns(UART_PORT_BASE + 8)); // 0x400
        assert!(!Uart8250::owns(UART_PORT_BASE - 1)); // 0x3F7
    }

    #[test]
    fn shadow_regs_and_dlab_reflect_writes() {
        let mut u = Uart8250::new();
        assert!(!u.dlab(), "DLAB clear at reset");
        // A benign register write (IER at offset 1) lands in the shadow array.
        u.write(UART_PORT_BASE + 1, 0xAB);
        assert_eq!(u.shadow_regs()[1], 0xAB);
        // Setting LCR bit 7 latches DLAB; the LCR shadow records the byte.
        u.write(UART_PORT_LCR, UART_LCR_DLAB);
        assert!(u.dlab());
        assert_eq!(u.shadow_regs()[3] & UART_LCR_DLAB, UART_LCR_DLAB);
        // Clearing it un-latches DLAB.
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

    // --- THRE interrupt (the COM1 IRQ-4 line) ------------------------------

    /// Port +2 read (IIR): no interrupt pending until the guest enables `IER.THRI`,
    /// then the THRE id — and never the FCR shadow written at the same port.
    #[test]
    fn iir_reports_thre_interrupt_only_when_thri_enabled() {
        let mut u = Uart8250::new();
        // Fresh: no interrupt enabled ⇒ IIR = NO_INT (0x01), line de-asserted.
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(0x01));
        assert!(!u.thre_irq_asserted());
        // A write to +2 is the FCR (FIFO control); it must NOT leak into the IIR
        // read (which would falsely clear NO_INT and is the original bug).
        u.write(UART_PORT_BASE + 2, 0xC7);
        assert_eq!(
            u.read(UART_PORT_BASE + 2),
            Some(0x01),
            "IIR is the interrupt status, not the FCR shadow"
        );
        assert!(!u.thre_irq_asserted());
        // Enable IER.THRI (DLAB clear): THRE is pending ⇒ IIR = 0x02, line asserts.
        u.write(UART_PORT_BASE + 1, UART_IER_THRI);
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(0x02));
        assert!(u.thre_irq_asserted());
        // A non-THRI IER bit (e.g. RDI 0x01, receive-data) does not assert TX: we
        // never feed input, so only THRE can be pending.
        u.write(UART_PORT_BASE + 1, 0x01);
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(0x01));
        assert!(!u.thre_irq_asserted());
        // Clearing THRI de-asserts (the kernel does this when the TX buffer drains).
        u.write(UART_PORT_BASE + 1, UART_IER_THRI);
        assert!(u.thre_irq_asserted());
        u.write(UART_PORT_BASE + 1, 0x00);
        assert!(!u.thre_irq_asserted());
    }

    /// With `DLAB` set, port +1 is the divisor-latch high byte (DLM), **not** the
    /// IER — so a `0x02` written there is the divisor, never `IER.THRI`, and it is a
    /// *separate* shadow that does not leak into the IER when DLAB is cleared.
    #[test]
    fn thri_ignored_while_dlab_selects_the_divisor_latch() {
        let mut u = Uart8250::new();
        u.write(UART_PORT_LCR, UART_LCR_DLAB); // DLAB = 1
        u.write(UART_PORT_BASE + 1, 0x02); // divisor-latch high (DLM) = 2
        assert!(
            !u.thre_irq_asserted(),
            "offset+1 is DLM here, not IER — no THRE assert"
        );
        // Offset +1 reads back the DLM (the byte just written), not the IER.
        assert_eq!(u.read(UART_PORT_BASE + 1), Some(0x02));
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(0x01));
        // Clearing DLAB re-exposes the IER, which was never written — so the DLM's
        // `0x02` does NOT leak into it and the line stays de-asserted.
        u.write(UART_PORT_LCR, 0x03); // DLAB = 0, 8N1
        assert!(
            !u.thre_irq_asserted(),
            "DLM must not leak into IER: a divisor write is not a THRI enable"
        );
        assert_eq!(u.read(UART_PORT_BASE + 1), Some(0x00), "IER unset, reads 0");
        // Now an actual IER.THRI write (DLAB clear) does assert.
        u.write(UART_PORT_BASE + 1, UART_IER_THRI);
        assert!(u.thre_irq_asserted());
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(0x02));
    }

    /// The codex nit: a divisor-latch (DLM) write in the DLAB window must **preserve
    /// the IER**. Enable `IER.THRI`, program the divisor (DLAB set → write DLL/DLM →
    /// DLAB clear), and confirm the THRE interrupt is still asserted afterwards — the
    /// DLM write to offset +1 did not clobber the IER. Kills the regression where
    /// offset +1 was a single shadow shared by IER and DLM.
    #[test]
    fn ier_preserved_across_divisor_latch_window() {
        let mut u = Uart8250::new();
        u.write(UART_PORT_BASE + 1, UART_IER_THRI); // IER = THRI (DLAB clear)
        assert!(u.thre_irq_asserted());
        // Program a non-trivial divisor in the DLAB window.
        u.write(UART_PORT_LCR, UART_LCR_DLAB); // DLAB = 1
        u.write(UART_PORT_BASE, 0x03); // DLL = 3
        u.write(UART_PORT_BASE + 1, 0x09); // DLM = 9 — must NOT touch the IER
        assert_eq!(u.read(UART_PORT_BASE + 1), Some(0x09), "DLM reads back");
        u.write(UART_PORT_LCR, 0x03); // DLAB = 0, 8N1
        // The IER survived the window intact: THRI still enabled, line still asserts.
        assert!(
            u.thre_irq_asserted(),
            "IER.THRI must survive a divisor-latch write"
        );
        assert_eq!(u.read(UART_PORT_BASE + 1), Some(UART_IER_THRI));
        assert_eq!(u.read(UART_PORT_BASE + 2), Some(UART_IIR_THRI));
    }

    /// The TX capture is unaffected by the interrupt machinery: a THR write still
    /// lands in `capture` whether or not the THRE interrupt is enabled.
    #[test]
    fn thr_capture_independent_of_thri() {
        let mut u = Uart8250::new();
        u.write(UART_PORT_BASE + 1, UART_IER_THRI); // enable THRE interrupt
        for &b in b"GUEST_READY" {
            assert!(u.write(UART_PORT_BASE, b));
        }
        assert_eq!(u.capture(), b"GUEST_READY");
    }

    // --- LegacyPlatform ----------------------------------------------------

    #[test]
    fn legacy_owns_the_curated_ports_only() {
        // PCI, PIC, PIT, i8042, CMOS, POST, ELCR, extra-COM are owned.
        for p in [
            0x0020, 0x0021, 0x00A0, 0x00A1, 0x0040, 0x0043, 0x0060, 0x0064, 0x0061, 0x0070, 0x0071,
            0x0080, 0x008F, 0x04D0, 0x04D1, 0x0CF8, 0x0CFB, 0x0CFC, 0x0CFF, 0x02F8, 0x03E8, 0x02E8,
        ] {
            assert!(LegacyPlatform::owns(p), "{p:#06x} should be owned");
        }
        // COM1 (the modeled UART), isa-debug-exit, report port, and random ports
        // are NOT the legacy stub's (COM1 is the real Uart8250; the others are
        // handled before the legacy fallback).
        for p in [0x03F8, 0x00F4, 0x0CA2, 0x0000, 0x1234, 0x0CF7, 0x0090] {
            assert!(!LegacyPlatform::owns(p), "{p:#06x} should not be owned");
        }
    }

    #[test]
    fn legacy_pci_config_address_round_trips_and_data_reads_no_device() {
        let mut p = LegacyPlatform::new();
        // A dword write to CONFIG_ADDRESS latches; a read returns it.
        p.write(0x0CF8, 4, 0x8000_1000);
        assert_eq!(p.config_address(), 0x8000_1000);
        assert_eq!(p.read(0x0CF8, 4), 0x8000_1000);
        // CONFIG_DATA reads "no device" (all-ones), masked to the access width.
        assert_eq!(p.read(0x0CFC, 4), 0xFFFF_FFFF);
        assert_eq!(p.read(0x0CFC, 2), 0x0000_FFFF);
        assert_eq!(p.read(0x0CFE, 1), 0x0000_00FF);
    }

    #[test]
    fn legacy_only_dword_write_to_cf8_latches() {
        // The latch fires ONLY for a 4-byte write to PCI_CONFIG_ADDRESS (0xCF8).
        // These pin the `size == 4 && port == 0xCF8` guard (kill `&&`→`||` and the
        // two `==`→`!=` mutants): neither a dword write to a *different* port nor a
        // non-dword write to 0xCF8 may touch the latch.
        let mut p = LegacyPlatform::new();
        p.write(0x0CFC, 4, 0xDEAD_BEEF); // dword, wrong port
        assert_eq!(
            p.config_address(),
            0,
            "dword write to non-CF8 must not latch"
        );
        p.write(0x0CF8, 1, 0xDEAD_BEEF); // right port, wrong size
        assert_eq!(p.config_address(), 0, "byte write to CF8 must not latch");
        p.write(0x0CF8, 2, 0xDEAD_BEEF); // right port, wrong size
        assert_eq!(p.config_address(), 0, "word write to CF8 must not latch");
        p.write(0x0CF8, 4, 0xCAFE_F00D); // the one combination that latches
        assert_eq!(p.config_address(), 0xCAFE_F00D);
    }

    #[test]
    fn legacy_reads_give_absent_idle_values() {
        let p = LegacyPlatform::new();
        // PIC IMRs reset all-masked (0xFF); an unpopulated COM port reads all-ones
        // (no UART).
        assert_eq!(p.read(0x0021, 1), 0xFF);
        assert_eq!(p.read(0x00A1, 1), 0xFF);
        assert_eq!(p.read(0x02F8, 1), 0xFF);
        // PIT, CMOS, POST, ELCR, PIC command, port B, i8042 data (0x60) read idle
        // (0). The i8042 *status* port (0x64) is the one exception — see
        // `i8042_status_reports_obf_set_so_the_probe_fails_fast`.
        for port in [0x0040, 0x0071, 0x0080, 0x04D0, 0x0020, 0x0061, 0x0060] {
            assert_eq!(p.read(port, 1), 0, "{port:#06x} should read idle");
        }
        // Non-PIC/PCI writes are accepted and dropped (no panic, no latched state).
        let mut p = p;
        p.write(0x0043, 1, 0x36); // program the PIT
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

    /// The i8042 **status** port (0x64) reports OBF-set / IBF-clear so the kernel's
    /// controller-presence check fails fast ("No controller found") instead of
    /// spinning `i8042_wait_read` 10000×`udelay` for an OBF that never comes —
    /// which strands the patched boot for minutes. The **data** port (0x60) stays
    /// idle (0). Exact bits are pinned so a flipped/zeroed constant is caught.
    #[test]
    fn i8042_status_reports_obf_set_so_the_probe_fails_fast() {
        let p = LegacyPlatform::new();
        let status = p.read(0x0064, 1);
        assert_eq!(status, 0x01, "0x64 status must read OBF-set, IBF-clear");
        // OBF (bit 0) set: makes i8042_flush drain its bounded buffer and report
        // "No controller found" — the controller probe never reaches the spinning
        // read-CTR command.
        assert_eq!(status & 0x01, 0x01, "OBF (bit 0) must be set");
        // IBF (bit 1) clear: any i8042_wait_write also completes immediately.
        assert_eq!(status & 0x02, 0x00, "IBF (bit 1) must be clear");
        // The data port (0x60) is unchanged — idle 0 (the drained bytes are
        // discarded by the flush).
        assert_eq!(p.read(0x0060, 1), 0, "0x60 data port stays idle");
        // The status is a constant (no state): it never latches and is unaffected
        // by writes, so two boots read it identically (determinism).
        let mut p = p;
        p.write(0x0064, 1, 0xFF); // a command-port write is accepted + dropped
        assert_eq!(p.read(0x0064, 1), 0x01, "status is stateless / constant");
    }

    #[test]
    fn pic_imr_latches_so_probe_8259a_sees_a_real_pic() {
        // The kernel's `probe_8259A` masks all of the slave, writes ~(1<<cascade)
        // (= 0xFB) to the master, and reads it back: a verbatim read-back means a
        // real PIC; an all-ones read means "Using NULL legacy PIC" (which strands
        // IRQ 4). The IMR is a latch, so the master read-back is exactly 0xFB
        // (not the old all-ones), and the slave read-back is the 0xFF it wrote.
        let mut p = LegacyPlatform::new();
        p.write(0x00A1, 1, 0xFF); // mask all of the slave
        p.write(0x0021, 1, 0xFB); // probe value (cascade IRQ 2 unmasked)
        assert_eq!(
            p.read(0x0021, 1),
            0xFB,
            "master IMR must read back verbatim"
        );
        assert_eq!(p.read(0x00A1, 1), 0xFF, "slave IMR must read back verbatim");
        assert_eq!(p.pic_imr(), [0xFB, 0xFF]);
        // Distinct latches: a master write does not touch the slave and vice versa.
        p.write(0x0021, 1, 0x12);
        assert_eq!(p.pic_imr(), [0x12, 0xFF]);
        p.write(0x00A1, 1, 0x34);
        assert_eq!(p.pic_imr(), [0x12, 0x34]);
    }

    #[test]
    fn irq_masked_reads_the_right_imr_bit() {
        let mut p = LegacyPlatform::new();
        // Fresh: every line masked.
        assert!(p.irq_masked(4), "IRQ 4 masked at reset");
        assert!(p.irq_masked(0));
        assert!(p.irq_masked(8));
        // Unmask only IRQ 4 (master bit 4 clear, the rest set) — the state after
        // the kernel `request_irq(4)`s the serial port.
        p.write(0x0021, 1, 0xFF & !(1 << 4));
        assert!(!p.irq_masked(4), "IRQ 4 now unmasked");
        assert!(
            p.irq_masked(3),
            "IRQ 3 still masked (kills off-by-one shift)"
        );
        assert!(p.irq_masked(5), "IRQ 5 still masked");
        assert!(p.irq_masked(0), "master line 0 untouched");
        // Slave lines come from the slave IMR (IRQ 8 = slave bit 0).
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
        // Out-of-range lines are treated as masked.
        assert!(p.irq_masked(16));
    }
}
