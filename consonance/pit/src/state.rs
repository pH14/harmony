// SPDX-License-Identifier: AGPL-3.0-or-later
//! Public constants (I/O ports, the input frequency) and the plain-data snapshot
//! structs [`PitState`] / [`PitCounterState`].

/// The i8254 input clock frequency in Hz — **1.193182 MHz**, the value
/// `docs/CPU-MSR-CONTRACT.md` fixes in its consistency chain ("the emulated PIT at
/// 1.193182 MHz of V-time"). The countdown decrements at this rate of V-time. This
/// is the historical PC value, `≈ 14.31818 MHz / 12` (the NTSC colorburst crystal
/// divided down). [`PitConfig`](crate::PitConfig) defaults the configured
/// frequency to this; the field exists so a test can vary it.
pub const PIT_FREQ_HZ: u64 = 1_193_182;

/// I/O port of counter 0 (read/write) — the **system timer**, whose terminal
/// count drives IRQ0.
pub const PIT_PORT_COUNTER0: u16 = 0x40;
/// I/O port of counter 1 (read/write) — historically DRAM refresh; modelled for
/// fidelity, raises no interrupt.
pub const PIT_PORT_COUNTER1: u16 = 0x41;
/// I/O port of counter 2 (read/write) — historically the PC speaker; modelled for
/// fidelity, raises no interrupt (its GATE at port `0x61` is tied high here).
pub const PIT_PORT_COUNTER2: u16 = 0x42;
/// I/O port of the mode/command register (write-only). Selects a counter + access
/// mode + counting mode + BCD/binary, issues a counter-latch command, or — with
/// the select field `0b11` — a read-back command.
pub const PIT_PORT_COMMAND: u16 = 0x43;

/// Format version of [`PitState`]. A future `vm-state` integration keys its
/// device-section decoding on this; bump it on any layout change.
pub const PIT_STATE_VERSION: u32 = 1;

/// Plain-data, versioned image of one i8254 counter — every field needed to
/// reproduce it observationally (same reads at every port for every `now_vns`,
/// same IRQ0 deadline, same firing decision).
///
/// Holds no `HashMap`/`HashSet` and no floats, so equal counter states always
/// produce bit-identical [`PitCounterState`] (a determinism requirement). The
/// firing deadline is **derived** (`arm_vns + ceil(N·1e9 / freq)`), never stored,
/// so absolute V-time deadlines survive restore unchanged.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PitCounterState {
    /// The mode field `M` exactly as programmed (the raw 3-bit value `0..=7`; `6`
    /// and `7` alias modes 2 and 3 in the counting behaviour).
    pub mode: u8,
    /// `true` if the counter counts in BCD (4 decimal digits), `false` for 16-bit
    /// binary (the Linux default).
    pub bcd: bool,
    /// Access mode (the `RW` field): `1` = lobyte only, `2` = hibyte only, `3` =
    /// lobyte-then-hibyte. (`0` is the latch *command*, never a stored access.)
    pub access: u8,
    /// The raw 16-bit reload register the guest last wrote (`0` means full scale —
    /// 65536 binary / 10000 BCD). Decoded for the tick arithmetic.
    pub reload: u16,
    /// V-time (ns) at which the counter was last (re)armed — a count write, or a
    /// periodic reload at a period boundary.
    pub arm_vns: u64,
    /// Whether a valid count has been loaded (the counter is counting). `false`
    /// after a control-word write that has not yet been followed by a count write.
    pub loaded: bool,
    /// Whether a **one-shot** mode (0/4) has reached terminal count and so will
    /// raise no further IRQ until reprogrammed. Always `false` for periodic modes.
    pub oneshot_fired: bool,
    /// The status NULL-count bit: `true` after a control word until the count is
    /// loaded (the count register does not yet hold a usable value).
    pub null_count: bool,
    /// lobyte/hibyte **write** flip-flop: `false` = the next counter write is the
    /// low byte (or the only byte), `true` = it is the high byte.
    pub write_phase: bool,
    /// The low byte held between the two writes of a lobyte/hibyte count.
    pub write_lo: u8,
    /// lobyte/hibyte **read** flip-flop: `false` = the next counter read returns
    /// the low byte, `true` = the high byte.
    pub read_phase: bool,
    /// Whether a counter-latch command has latched [`Self::latch_val`] for the next
    /// read(s); cleared once the latched value has been fully read out.
    pub count_latched: bool,
    /// The latched count (encoded as the raw register value), valid while
    /// [`Self::count_latched`].
    pub latch_val: u16,
    /// Whether a read-back command has latched [`Self::status_val`] for the next
    /// read; cleared once read out (status is read before any latched count).
    pub status_latched: bool,
    /// The latched status byte, valid while [`Self::status_latched`].
    pub status_val: u8,
}

/// Plain-data, versioned image of a [`Pit`](crate::Pit): the input frequency, the
/// pending-IRQ0 edge, and the three counters.
///
/// This is the struct a `vm-state` integration would embed in the snapshot blob
/// (vmm-core carries it in its device blob today). Every field is public so it can
/// be serialized field-by-field; with the `serde` feature it additionally derives
/// `Serialize`/`Deserialize`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct PitState {
    /// Snapshot format version; must equal [`PIT_STATE_VERSION`] on restore.
    pub version: u32,
    /// The frozen input frequency in Hz (the contract's [`PIT_FREQ_HZ`]). Must be
    /// non-zero; checked on restore.
    pub freq_hz: u64,
    /// Whether counter 0 has raised an IRQ0 edge the interrupt controller has not
    /// yet acknowledged ([`Pit::ack_irq0`](crate::Pit::ack_irq0)).
    pub irq0_pending: bool,
    /// The three counters, in order (0, 1, 2).
    pub counters: [PitCounterState; 3],
}
