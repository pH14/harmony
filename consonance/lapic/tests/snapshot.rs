// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — snapshot round-trip property test.
//!
//! Builds an arbitrary reachable LAPIC state (a random sequence of MMIO writes,
//! advances, raises, and deliveries), then asserts `snapshot()` → `restore()`
//! reproduces an observationally identical LAPIC: identical reads at every
//! register offset for several `now_vns` values, and identical
//! `next_timer_deadline` / `has_deliverable`. Also checks that `snapshot()` is
//! deterministic — equal states produce equal [`LapicState`].

use lapic::{
    APIC_DFR, APIC_EOI, APIC_ICR_HIGH, APIC_ICR_LOW, APIC_LDR, APIC_LVT_ERROR, APIC_LVT_LINT0,
    APIC_LVT_TIMER, APIC_MAX_OFFSET, APIC_PPR, APIC_SVR, APIC_TDCR, APIC_TMICT, APIC_TPR, Lapic,
    LapicConfig, LapicError,
};
use proptest::prelude::*;

/// `now_vns` values swept when comparing register reads across restore — spans
/// the arming instant, mid-count, and saturation extremes.
const SWEEP_TIMES: [u64; 7] = [
    0,
    1,
    1_000,
    1_000_000,
    1_000_000_000,
    u64::MAX / 2,
    u64::MAX,
];

#[derive(Clone, Debug)]
enum Op {
    Write { offset: u32, value: u32, now: u64 },
    Advance(u64),
    Raise(u8),
    Take,
    Eoi,
}

/// Offsets a guest realistically writes (a mix of writable registers plus a few
/// read-only ones, to exercise the deny-ignore-write path during state-building).
fn writable_offset() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(APIC_SVR),
        Just(APIC_TPR),
        Just(APIC_LDR),
        Just(APIC_DFR),
        Just(APIC_TDCR),
        Just(APIC_LVT_TIMER),
        Just(APIC_LVT_LINT0),
        Just(APIC_LVT_ERROR),
        Just(APIC_ICR_LOW),
        Just(APIC_ICR_HIGH),
        Just(APIC_TMICT),
        Just(APIC_EOI),
        Just(0x030), // Version: read-only, write must be a no-op
        Just(0x2F0), // CMCI: not modeled
    ]
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        (writable_offset(), any::<u32>(), 0u64..=2_000_000_000u64)
            .prop_map(|(offset, value, now)| Op::Write { offset, value, now }),
        (0u64..=4_000_000_000u64).prop_map(Op::Advance),
        (16u8..=255).prop_map(Op::Raise),
        Just(Op::Take),
        Just(Op::Eoi),
    ]
}

/// Apply a sequence of operations to a fresh LAPIC, returning the driven device.
fn drive(ops: &[Op], timer_hz: u64) -> Lapic {
    let mut l = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz,
    })
    .unwrap();
    for op in ops {
        match *op {
            Op::Write { offset, value, now } => {
                // Writes to known-good offsets never error.
                l.mmio_write(offset, value, now).unwrap();
            }
            Op::Advance(now) => {
                l.advance_to(now);
            }
            Op::Raise(v) => {
                l.raise(v).unwrap();
            }
            Op::Take => {
                l.take_interrupt();
            }
            Op::Eoi => l.eoi(),
        }
    }
    l
}

/// Assert two LAPICs are observationally identical.
fn assert_observationally_equal(a: &Lapic, b: &Lapic) -> Result<(), TestCaseError> {
    prop_assert_eq!(a.next_timer_deadline(), b.next_timer_deadline());
    prop_assert_eq!(a.has_deliverable(), b.has_deliverable());
    let mut offset = 0u32;
    while offset <= APIC_MAX_OFFSET {
        for &now in &SWEEP_TIMES {
            prop_assert_eq!(
                a.mmio_read(offset, now),
                b.mmio_read(offset, now),
                "offset {:#x} now {}",
                offset,
                now
            );
        }
        offset += 0x10;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(384))]

    #[test]
    fn snapshot_restore_observationally_identical(
        ops in prop::collection::vec(op_strategy(), 1..40),
        timer_hz in prop_oneof![Just(24_000_000u64), Just(25_000_000u64), 1u64..=4_000_000_000u64],
    ) {
        let l = drive(&ops, timer_hz);

        let snap = l.snapshot();
        let restored = Lapic::restore(&snap).expect("snapshot is internally consistent");

        // Restore reproduces an observationally identical device...
        assert_observationally_equal(&l, &restored)?;

        // ...and re-snapshotting the restored device yields the same bytes.
        prop_assert_eq!(restored.snapshot(), snap.clone());

        // snapshot() is deterministic: snapshotting twice is identical.
        prop_assert_eq!(l.snapshot(), snap);
    }

    /// Equal histories produce equal `LapicState` — the determinism requirement
    /// (no map iteration order, no float, no clock read leaks in).
    #[test]
    fn snapshot_is_deterministic_across_runs(
        ops in prop::collection::vec(op_strategy(), 1..40),
        timer_hz in 1u64..=4_000_000_000u64,
    ) {
        let a = drive(&ops, timer_hz);
        let b = drive(&ops, timer_hz);
        prop_assert_eq!(a.snapshot(), b.snapshot());
    }

    /// A spurious snapshot `version` is rejected by `restore`.
    #[test]
    fn restore_rejects_bad_version(bad in any::<u32>()) {
        let l = Lapic::new(LapicConfig { apic_id: 0, timer_hz: 25_000_000 }).unwrap();
        let mut snap = l.snapshot();
        prop_assume!(bad != snap.version);
        snap.version = bad;
        prop_assert!(Lapic::restore(&snap).is_err());
    }
}

/// Restore rejects structurally impossible timer bookkeeping (and a zero timer
/// frequency), and accepts a genuinely coherent armed snapshot.
#[test]
fn restore_rejects_inconsistent_timer() {
    let l = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz: 25_000_000,
    })
    .unwrap();

    // A pending count cannot be zero.
    let mut bad_pending = l.snapshot();
    bad_pending.timer_pending = true;
    bad_pending.initial_count = 0;
    assert!(Lapic::restore(&bad_pending).is_err());

    // Counting (running) requires the count to be pending.
    let mut bad_running = l.snapshot();
    bad_running.timer_running = true;
    bad_running.timer_pending = false;
    bad_running.initial_count = 100;
    assert!(Lapic::restore(&bad_running).is_err());

    // Running while the APIC is software-disabled / the LVT timer masked is
    // impossible (`timer_running` must equal armability). The fresh snapshot is
    // disabled+masked, so a running+pending timer is incoherent here.
    let mut bad_disabled = l.snapshot();
    bad_disabled.timer_running = true;
    bad_disabled.timer_pending = true;
    bad_disabled.initial_count = 100;
    assert!(Lapic::restore(&bad_disabled).is_err());

    // A zero timer frequency is rejected.
    let mut bad_hz = l.snapshot();
    bad_hz.timer_hz = 0;
    assert!(Lapic::restore(&bad_hz).is_err());

    // A genuinely coherent armed snapshot (enabled + unmasked one-shot, armed
    // via TMICT) is accepted.
    let mut armed = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz: 25_000_000,
    })
    .unwrap();
    armed
        .mmio_write(lapic::APIC_SVR, 0xFF | (1 << 8), 0)
        .unwrap(); // enable
    armed.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap(); // unmasked one-shot
    armed.mmio_write(APIC_TMICT, 1000, 0).unwrap(); // arm
    let good = armed.snapshot();
    assert!(good.timer_running && good.timer_pending);
    assert!(Lapic::restore(&good).is_ok());
}

/// `restore` enforces the running-timer anchor bound: a running timer's
/// `count_at_arm` is the full load or a re-anchored remainder, so it is **never
/// more than** `initial_count`. The boundary `count_at_arm == initial_count` (the
/// normal fresh-arm state) must be accepted; one tick over is unreachable through
/// the MMIO paths and must be rejected. Pins the `count_at_arm > initial_count`
/// restore guard (`src/device.rs`) — without this, a `> -> <` mutation of that
/// comparison survives.
#[test]
fn restore_enforces_anchor_count_bound() {
    // A genuinely coherent running one-shot, armed via TMICT: at a fresh arm the
    // anchor count equals the loaded initial count.
    let mut armed = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz: 25_000_000,
    })
    .unwrap();
    armed.mmio_write(APIC_SVR, 0xFF | (1 << 8), 0).unwrap(); // enable
    armed.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap(); // unmasked one-shot
    armed.mmio_write(APIC_TMICT, 1000, 0).unwrap(); // arm
    let good = armed.snapshot();
    assert!(good.timer_running && good.timer_pending);

    // (b) Boundary: count_at_arm == initial_count is accepted ("never MORE than").
    assert_eq!(good.count_at_arm, good.initial_count);
    assert!(Lapic::restore(&good).is_ok());

    // (a) One over the loaded count is unreachable -> rejected. This is the
    // assertion that kills the `> -> <` mutant: under the mutation the guard reads
    // `count_at_arm < initial_count`, which is false here, so restore would wrongly
    // accept.
    let mut over = good.clone();
    over.count_at_arm = over.initial_count + 1;
    assert_eq!(Lapic::restore(&over).unwrap_err(), LapicError::InvalidState);
}

/// The `PPR` offset constant resolves and is read-only (sanity that the public
/// constant set is wired correctly for downstream callers).
#[test]
fn ppr_offset_is_exposed() {
    let l = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz: 25_000_000,
    })
    .unwrap();
    assert_eq!(l.mmio_read(APIC_PPR, 0), Ok(0));
}
