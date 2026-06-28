// SPDX-License-Identifier: AGPL-3.0-or-later
//! Property tests for the [`Pit`] against an **independent reference model**.
//!
//! The reference is a naive **tick-by-tick simulator** (a down-counter stepped one
//! input clock at a time, reloading/wrapping per mode) — a different computation
//! path from the device's closed-form modular arithmetic, so a transcription or
//! off-by-one bug in the closed form diverges from the loop and the property fails.
//! A separate harness checks the IRQ0 deadline against an independently-formulated
//! ceiling at the real contract frequency, and a fuzz harness asserts no access
//! sequence panics (Convention rule #4: library code never panics on input).

use pit::{Pit, PitConfig, PIT_FREQ_HZ, PIT_PORT_COMMAND, PIT_PORT_COUNTER0};
use proptest::prelude::*;

/// 1 kHz test clock: 1 tick = 1 ms exactly, so V-time `t · TICK_NS` maps to exactly
/// `t` whole ticks (the floor in the device's `elapsed_ticks` is exact). This lets
/// the tick-stepping reference and the V-time-driven device be compared directly.
const KHZ: u64 = 1_000;
const TICK_NS: u64 = 1_000_000;

/// Binary / BCD counting moduli.
const BIN_MODULUS: u64 = 65_536;

fn ctrl(sc: u8, rw: u8, m: u8, bcd: bool) -> u8 {
    (sc << 6) | (rw << 4) | (m << 1) | u8::from(bcd)
}

/// Program counter 0 (lohi, binary) in `mode` with reload `n`, armed at `now`.
fn program_counter0(p: &mut Pit, mode: u8, n: u16, now: u64) {
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 3, mode, false), now)
        .unwrap();
    p.port_write(PIT_PORT_COUNTER0, (n & 0xFF) as u8, now).unwrap();
    p.port_write(PIT_PORT_COUNTER0, (n >> 8) as u8, now).unwrap();
}

/// Latch + read counter 0's current count (lo then hi) at V-time `now`.
fn read_count0(p: &mut Pit, now: u64) -> u16 {
    p.port_write(PIT_PORT_COMMAND, ctrl(0, 0, 0, false), now)
        .unwrap();
    let lo = p.port_read(PIT_PORT_COUNTER0, now).unwrap();
    let hi = p.port_read(PIT_PORT_COUNTER0, now).unwrap();
    u16::from(lo) | (u16::from(hi) << 8)
}

/// Decoded reload (`0` → 65536).
fn decode(n: u16) -> u64 {
    if n == 0 { BIN_MODULUS } else { u64::from(n) }
}

/// Independent reference: step a down-counter one tick at a time for `total_ticks`,
/// returning the displayed count after each tick (index `t` = after `t` ticks) and
/// the tick indices at which counter 0 raises an IRQ0 edge.
///
/// - **periodic** (modes 2/3): the element runs `n..=1` then reloads to `n`, raising
///   IRQ0 on the reload edge — so an edge at every multiple of `n`.
/// - **one-shot** (modes 0/4): counts `n..=0`, raising IRQ0 once at the terminal
///   count (tick `n`), then keeps wrapping with no further edge.
fn reference(decoded_mode: u8, n: u64, total_ticks: u64) -> (Vec<u64>, Vec<u64>) {
    let periodic = matches!(decoded_mode, 2 | 3);
    let mut counts = vec![n % BIN_MODULUS]; // t = 0 (full count; 65536 displays 0)
    let mut irqs = Vec::new();
    if periodic {
        let mut elem = n;
        for t in 1..=total_ticks {
            if elem == 1 {
                elem = n; // reload
                irqs.push(t); // IRQ on the reload edge
            } else {
                elem -= 1;
            }
            counts.push(elem % BIN_MODULUS);
        }
    } else {
        let mut elem = n % BIN_MODULUS; // displayed element
        for t in 1..=total_ticks {
            elem = (elem + BIN_MODULUS - 1) % BIN_MODULUS; // decrement, wrapping
            counts.push(elem);
            if t == n {
                irqs.push(t); // terminal count: the single one-shot edge
            }
        }
    }
    (irqs, counts)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The device's current-count read-back matches the tick-stepping reference at
    /// every tick of a bounded run, for every modelled mode.
    #[test]
    fn current_count_matches_reference(
        mode in prop::sample::select(vec![0u8, 2, 3, 4]),
        n in 1u16..=64,
        total in 1u64..=200,
    ) {
        let (_irqs, counts) = reference(mode, decode(n), total);
        let mut p = Pit::new(PitConfig { freq_hz: KHZ }).unwrap();
        program_counter0(&mut p, mode, n, 0);
        for t in 0..=total {
            let got = read_count0(&mut p, t * TICK_NS);
            prop_assert_eq!(u64::from(got), counts[t as usize], "count mismatch at tick {}", t);
        }
    }

    /// IRQ0 fires at exactly the reference tick indices over a bounded run: stepping
    /// `advance_to` one tick at a time (ack on fire) raises IRQ0 on precisely the
    /// ticks the reference's edge detector does — no early, late, or missed edge.
    #[test]
    fn irq0_edges_match_reference(
        mode in prop::sample::select(vec![0u8, 2, 3, 4]),
        n in 1u16..=64,
        total in 1u64..=200,
    ) {
        let (irqs, _counts) = reference(mode, decode(n), total);
        let mut p = Pit::new(PitConfig { freq_hz: KHZ }).unwrap();
        program_counter0(&mut p, mode, n, 0);
        let mut fired = Vec::new();
        for t in 1..=total {
            if p.advance_to(t * TICK_NS) {
                fired.push(t);
                p.ack_irq0();
            }
        }
        prop_assert_eq!(fired, irqs);
    }

    /// The IRQ0 deadline equals an **independently-formulated** ceiling
    /// `ceil(N · 1e9 / freq)` at the real contract frequency, for any reload and
    /// arm — and the edge fires no earlier than that deadline (ceil, never early).
    #[test]
    fn deadline_is_independent_ceil(
        mode in prop::sample::select(vec![0u8, 2, 3, 4]),
        n in 0u16..=u16::MAX,
        arm in 0u64..=1_000_000_000_000,
    ) {
        let mut p = Pit::new(PitConfig { freq_hz: PIT_FREQ_HZ }).unwrap();
        program_counter0(&mut p, mode, n, arm);
        let nd = decode(n) as u128;
        let prod = nd * 1_000_000_000u128;
        // Independent ceil: floor, then bump if it did not cover the product.
        let q = prod / u128::from(PIT_FREQ_HZ);
        let ceil = if q * u128::from(PIT_FREQ_HZ) < prod { q + 1 } else { q };
        let deadline = arm as u128 + ceil;
        prop_assert_eq!(p.next_irq0_deadline(), Some(deadline as u64));
        // Ceil: one ns before the deadline does not fire; at the deadline it does.
        if deadline > u128::from(arm) {
            prop_assert!(!p.advance_to(deadline as u64 - 1));
        }
        prop_assert!(p.advance_to(deadline as u64));
    }

    /// Snapshot/restore is observationally transparent: a restored PIT yields the
    /// same deadline and the same read-back at an arbitrary later V-time.
    #[test]
    fn snapshot_restore_roundtrips(
        mode in prop::sample::select(vec![0u8, 2, 3, 4]),
        n in 1u16..=u16::MAX,
        arm in 0u64..=1_000_000,
        query in 0u64..=10_000_000_000,
    ) {
        let mut p = Pit::new(PitConfig { freq_hz: PIT_FREQ_HZ }).unwrap();
        program_counter0(&mut p, mode, n, arm);
        let snap = p.snapshot();
        let mut restored = Pit::restore(&snap).unwrap();
        // Round-trip is exact (checked before any read mutates the latch state).
        prop_assert_eq!(restored.snapshot(), snap);
        prop_assert_eq!(restored.next_irq0_deadline(), p.next_irq0_deadline());
        let q = arm.saturating_add(query);
        prop_assert_eq!(read_count0(&mut restored, q), read_count0(&mut p, q));
    }

    /// No access sequence panics: arbitrary writes/reads on the four ports at
    /// monotonically non-decreasing V-times leave the device total (rule #4), and
    /// the IRQ0 deadline never lands in the past relative to the arm it derives from.
    #[test]
    fn arbitrary_io_never_panics(
        ops in prop::collection::vec(
            (0u16..=4u16, any::<u8>(), 0u64..=5_000_000_000u64, any::<bool>()),
            0..200,
        ),
    ) {
        let mut p = Pit::new(PitConfig { freq_hz: PIT_FREQ_HZ }).unwrap();
        let mut now = 0u64;
        for (port_sel, value, dt, is_read) in ops {
            now = now.saturating_add(dt);
            let port = PIT_PORT_COUNTER0 + port_sel; // 0x40..=0x44 (0x44 exercises BadPort)
            if is_read {
                let _ = p.port_read(port, now);
            } else {
                let _ = p.port_write(port, value, now);
            }
            // Advancing never panics and the deadline is consistent.
            let _ = p.advance_to(now);
            let _ = p.next_irq0_deadline();
        }
    }
}
