// SPDX-License-Identifier: AGPL-3.0-or-later
//! Unit tests for the [`Pit`] state machine. A 1 kHz test frequency is used where
//! exact V-time arithmetic matters (1 tick = 1 ms exactly), with one test at the
//! real [`PIT_FREQ_HZ`] for the contract value.

use super::*;
use crate::state::{PIT_PORT_COUNTER1, PIT_PORT_COUNTER2};

/// 1 kHz: 1 tick = 1_000_000 ns, so `period_ns(N) == N · 1_000_000` exactly.
const KHZ: u64 = 1_000;
/// One tick at [`KHZ`], in ns.
const TICK_NS: u64 = 1_000_000;

fn pit(freq: u64) -> Pit {
    Pit::new(PitConfig { freq_hz: freq }).expect("valid config")
}

/// Control word: counter `sc`, access `rw`, mode `m`, binary/BCD `bcd`.
fn ctrl(sc: u8, rw: u8, m: u8, bcd: bool) -> u8 {
    (sc << 6) | (rw << 4) | (m << 1) | u8::from(bcd)
}

/// Program counter 0, mode 2 (rate generator), lobyte/hibyte, binary, reload `n`,
/// armed at `now`.
fn arm_counter0_mode2(p: &mut Pit, n: u16, now: u64) {
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, 2, false), now)
        .unwrap();
    p.port_write(PIT_PORT_COUNTER0, (n & 0xFF) as u8, now)
        .unwrap();
    p.port_write(PIT_PORT_COUNTER0, (n >> 8) as u8, now)
        .unwrap();
}

#[test]
fn rejects_zero_frequency() {
    assert_eq!(
        Pit::new(PitConfig { freq_hz: 0 }).unwrap_err(),
        PitError::ZeroFrequency
    );
}

#[test]
fn bad_port_rejected() {
    let mut p = pit(KHZ);
    assert_eq!(p.port_write(0x44, 0, 0), Err(PitError::BadPort(0x44)));
    assert_eq!(p.port_read(0x3F, 0), Err(PitError::BadPort(0x3F)));
}

#[test]
fn command_port_reads_zero() {
    let mut p = pit(KHZ);
    // The command register is write-only.
    assert_eq!(p.port_read(PIT_PORT_COMMAND, 0), Ok(0));
}

#[test]
fn unloaded_counter_has_no_deadline_and_no_fire() {
    let mut p = pit(KHZ);
    assert_eq!(p.next_irq0_deadline(), None);
    assert!(!p.advance_to(u64::MAX));
    assert!(!p.irq0_pending());
    // A control word alone (no count write) does not arm.
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, 2, false), 0)
        .unwrap();
    assert_eq!(p.next_irq0_deadline(), None);
}

#[test]
fn mode2_fires_at_exact_vtime_and_reloads() {
    let mut p = pit(KHZ);
    arm_counter0_mode2(&mut p, 10, 0); // period = 10 ms
    assert_eq!(p.next_irq0_deadline(), Some(10 * TICK_NS));

    // One ns before the deadline: no fire.
    assert!(!p.advance_to(10 * TICK_NS - 1));
    assert!(!p.irq0_pending());
    assert_eq!(p.next_irq0_deadline(), Some(10 * TICK_NS));

    // Exactly at the deadline: IRQ0 raised, re-armed one period ahead.
    assert!(p.advance_to(10 * TICK_NS));
    assert!(p.irq0_pending());
    assert_eq!(p.next_irq0_deadline(), Some(20 * TICK_NS));

    // Acknowledge; the next period still pending.
    p.ack_irq0();
    assert!(!p.irq0_pending());
    assert!(p.advance_to(20 * TICK_NS));
    assert!(p.irq0_pending());
    assert_eq!(p.next_irq0_deadline(), Some(30 * TICK_NS));
}

#[test]
fn advance_is_idempotent_at_a_fixed_now() {
    let mut p = pit(KHZ);
    arm_counter0_mode2(&mut p, 5, 0);
    assert!(p.advance_to(5 * TICK_NS)); // fires
    p.ack_irq0();
    // A repeat call at the same now must NOT re-fire (the period boundary moved).
    assert!(!p.advance_to(5 * TICK_NS));
    assert!(!p.irq0_pending());
}

#[test]
fn periodic_catches_up_missed_periods_closed_form() {
    let mut p = pit(KHZ);
    arm_counter0_mode2(&mut p, 4, 0); // period = 4 ms
    // Jump 3.5 periods ahead in one advance: fires once (edge-coalesced), re-armed
    // to the last boundary at-or-before now (12 ms), next deadline 16 ms.
    assert!(p.advance_to(14 * TICK_NS));
    assert_eq!(p.next_irq0_deadline(), Some(16 * TICK_NS));
}

#[test]
fn mode0_oneshot_fires_once_then_silent() {
    let mut p = pit(KHZ);
    // Counter 0, mode 0 (interrupt on terminal count), lohi, binary, N = 8.
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, 0, false), 0)
        .unwrap();
    p.port_write(PIT_PORT_COUNTER0, 8, 0).unwrap();
    p.port_write(PIT_PORT_COUNTER0, 0, 0).unwrap();
    assert_eq!(p.next_irq0_deadline(), Some(8 * TICK_NS));

    assert!(p.advance_to(8 * TICK_NS));
    assert!(p.irq0_pending());
    // A one-shot is consumed: no further deadline, no further fire.
    assert_eq!(p.next_irq0_deadline(), None);
    p.ack_irq0();
    assert!(!p.advance_to(100 * TICK_NS));
    assert!(!p.irq0_pending());

    // Reprogramming re-arms it.
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, 0, false), 8 * TICK_NS)
        .unwrap();
    p.port_write(PIT_PORT_COUNTER0, 8, 8 * TICK_NS).unwrap();
    p.port_write(PIT_PORT_COUNTER0, 0, 8 * TICK_NS).unwrap();
    assert_eq!(p.next_irq0_deadline(), Some(16 * TICK_NS));
}

#[test]
fn mode3_square_wave_is_periodic() {
    let mut p = pit(KHZ);
    // Counter 0, mode 3 (square wave), lohi, binary, N = 6.
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, 3, false), 0)
        .unwrap();
    p.port_write(PIT_PORT_COUNTER0, 6, 0).unwrap();
    p.port_write(PIT_PORT_COUNTER0, 0, 0).unwrap();
    assert_eq!(p.next_irq0_deadline(), Some(6 * TICK_NS));
    assert!(p.advance_to(6 * TICK_NS));
    p.ack_irq0();
    assert_eq!(
        p.next_irq0_deadline(),
        Some(12 * TICK_NS),
        "reloads, period N"
    );
}

#[test]
fn mode6_and_7_alias_modes_2_and_3() {
    // Mode field 6 aliases rate-generator (2); 7 aliases square-wave (3) — both
    // periodic, so a future deadline exists and reloads.
    for m in [6u8, 7u8] {
        let mut p = pit(KHZ);
        p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, m, false), 0)
            .unwrap();
        p.port_write(PIT_PORT_COUNTER0, 4, 0).unwrap();
        p.port_write(PIT_PORT_COUNTER0, 0, 0).unwrap();
        assert_eq!(
            p.next_irq0_deadline(),
            Some(4 * TICK_NS),
            "mode {m} periodic"
        );
        assert!(p.advance_to(4 * TICK_NS));
        p.ack_irq0();
        assert_eq!(p.next_irq0_deadline(), Some(8 * TICK_NS));
    }
}

#[test]
fn gate_modes_1_and_5_raise_no_irq() {
    // Modes 1/5 are GATE-triggered; counter 0's gate is tied high, so they never
    // fire — no deadline, no IRQ.
    for m in [1u8, 5u8] {
        let mut p = pit(KHZ);
        p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, m, false), 0)
            .unwrap();
        p.port_write(PIT_PORT_COUNTER0, 4, 0).unwrap();
        p.port_write(PIT_PORT_COUNTER0, 0, 0).unwrap();
        assert_eq!(p.next_irq0_deadline(), None, "mode {m} is gate-triggered");
        assert!(!p.advance_to(1_000 * TICK_NS));
    }
}

#[test]
fn lobyte_hibyte_latch_read_roundtrip() {
    let mut p = pit(KHZ);
    arm_counter0_mode2(&mut p, 0x1234, 0); // period N = 0x1234
    // Latch the count at t=0 (full reload N): a stable lo/hi read pair.
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 0, 0, false), 0)
        .unwrap(); // rw=0 ⇒ latch
    let lo = p.port_read(PIT_PORT_COUNTER0, 0).unwrap();
    let hi = p.port_read(PIT_PORT_COUNTER0, 0).unwrap();
    assert_eq!(u16::from(lo) | (u16::from(hi) << 8), 0x1234);
    // Latch is consumed; a fresh latch at a later time reflects the decremented count.
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 0, 0, false), 4 * TICK_NS)
        .unwrap();
    let lo = p.port_read(PIT_PORT_COUNTER0, 4 * TICK_NS).unwrap();
    let hi = p.port_read(PIT_PORT_COUNTER0, 4 * TICK_NS).unwrap();
    assert_eq!(
        u16::from(lo) | (u16::from(hi) << 8),
        0x1234 - 4,
        "count decremented by 4 ticks"
    );
}

#[test]
fn lobyte_only_access_mode() {
    let mut p = pit(KHZ);
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 1, 2, false), 0)
        .unwrap(); // rw=1 lobyte
    p.port_write(PIT_PORT_COUNTER0, 0x55, 0).unwrap();
    assert_eq!(p.next_irq0_deadline(), Some(0x55 * TICK_NS));
    // Each read returns the low byte of the current count (latched here for stability).
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 0, 0, false), 0)
        .unwrap();
    assert_eq!(p.port_read(PIT_PORT_COUNTER0, 0).unwrap(), 0x55);
}

#[test]
fn hibyte_only_access_mode() {
    let mut p = pit(KHZ);
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 2, 2, false), 0)
        .unwrap(); // rw=2 hibyte
    p.port_write(PIT_PORT_COUNTER0, 0x12, 0).unwrap(); // reload = 0x1200
    assert_eq!(p.next_irq0_deadline(), Some(0x1200 * TICK_NS));
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 0, 0, false), 0)
        .unwrap();
    assert_eq!(
        p.port_read(PIT_PORT_COUNTER0, 0).unwrap(),
        0x12,
        "high byte"
    );
}

#[test]
fn current_count_decrements_over_vtime() {
    let mut p = pit(KHZ);
    arm_counter0_mode2(&mut p, 100, 0);
    // Read-back via latch at successive times.
    for t in [0u64, 10, 50, 99] {
        p.port_write(PIT_PORT_COMMAND, ctrl(0, 0, 0, false), t * TICK_NS)
            .unwrap();
        let lo = p.port_read(PIT_PORT_COUNTER0, t * TICK_NS).unwrap();
        let hi = p.port_read(PIT_PORT_COUNTER0, t * TICK_NS).unwrap();
        let count = u16::from(lo) | (u16::from(hi) << 8);
        // Rate generator: count = N − (ticks mod N), never 0.
        let expected = if t == 0 { 100 } else { 100 - t as u16 };
        assert_eq!(count, expected, "at t={t}");
    }
}

#[test]
fn readback_command_latches_count_and_status() {
    let mut p = pit(KHZ);
    arm_counter0_mode2(&mut p, 0x0042, 0);
    // Read-back: latch count + status of counter 0 (bits 5,4 clear; bit1 selects c0).
    // value = 11_0_0_001_0 = 0xC2.
    let rb = 0b1100_0010;
    p.port_write(PIT_PORT_COMMAND, rb, 0).unwrap();
    // Status is read first, then the latched count (lo, hi).
    let status = p.port_read(PIT_PORT_COUNTER0, 0).unwrap();
    // Status: RW=3 (bits 5:4), mode=2 (bits 3:1), BCD=0, NULL clear (count loaded).
    assert_eq!(status & 0b0011_1111, (3 << 4) | (2 << 1));
    let lo = p.port_read(PIT_PORT_COUNTER0, 0).unwrap();
    let hi = p.port_read(PIT_PORT_COUNTER0, 0).unwrap();
    assert_eq!(u16::from(lo) | (u16::from(hi) << 8), 0x0042);
}

#[test]
fn null_count_bit_set_until_count_loaded() {
    let mut p = pit(KHZ);
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, 2, false), 0)
        .unwrap();
    // Read-back status only (bit4 clear ⇒ status, bit5 set ⇒ no count): 11_1_0_001_0.
    p.port_write(PIT_PORT_COMMAND, 0b1110_0010, 0).unwrap();
    let status = p.port_read(PIT_PORT_COUNTER0, 0).unwrap();
    assert_ne!(status & (1 << 6), 0, "NULL count set before a count write");
    // Load the count; NULL clears.
    p.port_write(PIT_PORT_COUNTER0, 4, 0).unwrap();
    p.port_write(PIT_PORT_COUNTER0, 0, 0).unwrap();
    p.port_write(PIT_PORT_COMMAND, 0b1110_0010, 0).unwrap();
    let status = p.port_read(PIT_PORT_COUNTER0, 0).unwrap();
    assert_eq!(status & (1 << 6), 0, "NULL count clear after a count write");
}

#[test]
fn bcd_mode_counts_in_bcd() {
    let mut p = pit(KHZ);
    // Counter 0, mode 2, lohi, BCD; reload 0x0100 BCD = 100 decimal.
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, 2, true), 0)
        .unwrap();
    p.port_write(PIT_PORT_COUNTER0, 0x00, 0).unwrap();
    p.port_write(PIT_PORT_COUNTER0, 0x01, 0).unwrap();
    // 100 decimal ticks → period 100 ms.
    assert_eq!(p.next_irq0_deadline(), Some(100 * TICK_NS));
    // After 1 tick, count = 99 decimal = 0x0099 BCD.
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 0, 0, true), TICK_NS)
        .unwrap();
    let lo = p.port_read(PIT_PORT_COUNTER0, TICK_NS).unwrap();
    let hi = p.port_read(PIT_PORT_COUNTER0, TICK_NS).unwrap();
    assert_eq!(u16::from(lo) | (u16::from(hi) << 8), 0x0099);
}

#[test]
fn only_counter0_raises_irq0() {
    for port in [PIT_PORT_COUNTER1, PIT_PORT_COUNTER2] {
        let sc = (port - PIT_PORT_COUNTER0) as u8;
        let mut p = pit(KHZ);
        p.port_write(PIT_PORT_COMMAND, ctrl(sc, 3, 2, false), 0)
            .unwrap();
        p.port_write(port, 4, 0).unwrap();
        p.port_write(port, 0, 0).unwrap();
        // Counters 1/2 are not interrupt sources: no IRQ0 deadline, no fire.
        assert_eq!(p.next_irq0_deadline(), None, "counter {sc} no IRQ0");
        assert!(!p.advance_to(100 * TICK_NS));
        assert!(!p.irq0_pending());
    }
}

#[test]
fn real_frequency_deadline_is_ceil() {
    let mut p = pit(PIT_FREQ_HZ);
    // HZ=100 style reload.
    let n = 11_932u16;
    arm_counter0_mode2(&mut p, n, 0);
    // period = ceil(11932 · 1e9 / 1_193_182).
    let expect = (u128::from(n) * 1_000_000_000u128).div_ceil(u128::from(PIT_FREQ_HZ)) as u64;
    assert_eq!(p.next_irq0_deadline(), Some(expect));
    // Fires no earlier than `expect` (ceil): one ns before is silent.
    assert!(!p.advance_to(expect - 1));
    assert!(p.advance_to(expect));
}

#[test]
fn snapshot_restore_roundtrips() {
    let mut p = pit(KHZ);
    arm_counter0_mode2(&mut p, 50, 0);
    p.advance_to(50 * TICK_NS); // fire + re-arm
    let snap = p.snapshot();
    let restored = Pit::restore(&snap).expect("restore");
    // Observationally identical: same deadline, same pending, same snapshot.
    assert_eq!(restored.next_irq0_deadline(), p.next_irq0_deadline());
    assert_eq!(restored.irq0_pending(), p.irq0_pending());
    assert_eq!(restored.snapshot(), snap);
}

#[test]
fn restore_rejects_bad_state() {
    let mut p = pit(KHZ);
    arm_counter0_mode2(&mut p, 10, 0);
    let good = p.snapshot();

    let mut bad_version = good;
    bad_version.version = good.version + 1;
    assert_eq!(
        Pit::restore(&bad_version).unwrap_err(),
        PitError::InvalidState
    );

    let mut bad_freq = good;
    bad_freq.freq_hz = 0;
    assert_eq!(Pit::restore(&bad_freq).unwrap_err(), PitError::InvalidState);

    let mut bad_access = good;
    bad_access.counters[0].access = 7; // not 0..=3
    assert_eq!(
        Pit::restore(&bad_access).unwrap_err(),
        PitError::InvalidState
    );

    let mut bad_mode = good;
    bad_mode.counters[0].mode = 9; // not 0..=7
    assert_eq!(Pit::restore(&bad_mode).unwrap_err(), PitError::InvalidState);
}

#[test]
fn restored_far_future_arm_saturates_to_no_deadline() {
    // A reload whose deadline would exceed u64 (arm near u64::MAX) yields None
    // rather than a clamped deadline that never fires — the saturating contract.
    let mut snap = pit(KHZ).snapshot();
    snap.counters[0] = PitCounterState {
        mode: 2,
        bcd: false,
        access: 3,
        reload: 100,
        arm_vns: u64::MAX,
        loaded: true,
        oneshot_fired: false,
        null_count: false,
        ..Default::default()
    };
    let p = Pit::restore(&snap).expect("restore");
    assert_eq!(p.next_irq0_deadline(), None, "deadline beyond u64 ⇒ None");
}

#[test]
fn control_word_resets_read_write_flip_flops() {
    let mut p = pit(KHZ);
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, 2, false), 0)
        .unwrap();
    // Write only the low byte (write flip-flop now expects the high byte)...
    p.port_write(PIT_PORT_COUNTER0, 0x99, 0).unwrap();
    assert_eq!(p.next_irq0_deadline(), None, "half-written count not armed");
    // ...then a fresh control word resets the flip-flop, so the next two writes are
    // a clean lo/hi pair.
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, 2, false), 0)
        .unwrap();
    p.port_write(PIT_PORT_COUNTER0, 0x10, 0).unwrap();
    p.port_write(PIT_PORT_COUNTER0, 0x00, 0).unwrap();
    assert_eq!(p.next_irq0_deadline(), Some(0x10 * TICK_NS));
}

#[test]
fn ack_without_pending_is_noop() {
    let mut p = pit(KHZ);
    p.ack_irq0();
    assert!(!p.irq0_pending());
}

#[test]
fn bcd_all_four_digits_roundtrip() {
    let mut p = pit(KHZ);
    // BCD reload 0x4321 = 4321 decimal — exercises all four BCD digit positions.
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, 2, true), 0).unwrap();
    p.port_write(PIT_PORT_COUNTER0, 0x21, 0).unwrap();
    p.port_write(PIT_PORT_COUNTER0, 0x43, 0).unwrap();
    assert_eq!(p.next_irq0_deadline(), Some(4321 * TICK_NS), "decoded 4321 ticks");
    // After 321 ticks, count = 4000 decimal = 0x4000 BCD (each digit re-encoded).
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 0, 0, true), 321 * TICK_NS)
        .unwrap();
    let lo = p.port_read(PIT_PORT_COUNTER0, 321 * TICK_NS).unwrap();
    let hi = p.port_read(PIT_PORT_COUNTER0, 321 * TICK_NS).unwrap();
    assert_eq!(u16::from(lo) | (u16::from(hi) << 8), 0x4000);
}

#[test]
fn status_output_bit_reflects_waveform_phase() {
    // Read the status byte (bit 7 = OUTPUT pin) via a read-back status-latch command
    // (bits 7,6 = readback; bit 5 set = no count latch; bit 4 clear = latch status;
    // bit 1 = select counter 0): value 0b1110_0010.
    fn output(p: &mut Pit, now: u64) -> u8 {
        p.port_write(PIT_PORT_COMMAND, 0b1110_0010, now).unwrap();
        p.port_read(PIT_PORT_COUNTER0, now).unwrap() & 0x80
    }

    // Mode 0 (one-shot): OUT low until terminal count, then high.
    let mut p = pit(KHZ);
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, 0, false), 0).unwrap();
    p.port_write(PIT_PORT_COUNTER0, 10, 0).unwrap();
    p.port_write(PIT_PORT_COUNTER0, 0, 0).unwrap();
    assert_eq!(output(&mut p, 5 * TICK_NS), 0, "mode 0 OUT low before terminal");
    assert_eq!(output(&mut p, 10 * TICK_NS), 0x80, "mode 0 OUT high at terminal");

    // Mode 2 (rate generator): OUT high except the single clock at count 1.
    let mut p = pit(KHZ);
    arm_counter0_mode2(&mut p, 10, 0);
    assert_eq!(output(&mut p, 0), 0x80, "mode 2 OUT high at full count");
    assert_eq!(output(&mut p, 9 * TICK_NS), 0, "mode 2 OUT low at count 1");

    // Mode 3 (square wave): OUT high the first half of the period, low the second.
    let mut p = pit(KHZ);
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, 3, false), 0).unwrap();
    p.port_write(PIT_PORT_COUNTER0, 10, 0).unwrap();
    p.port_write(PIT_PORT_COUNTER0, 0, 0).unwrap();
    assert_eq!(output(&mut p, 2 * TICK_NS), 0x80, "mode 3 OUT high first half");
    assert_eq!(output(&mut p, 7 * TICK_NS), 0, "mode 3 OUT low second half");
}
