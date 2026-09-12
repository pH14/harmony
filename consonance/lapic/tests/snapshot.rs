// SPDX-License-Identifier: AGPL-3.0-or-later

use lapic::{
    APIC_DFR, APIC_EOI, APIC_ICR_HIGH, APIC_ICR_LOW, APIC_LDR, APIC_LVT_ERROR, APIC_LVT_LINT0,
    APIC_LVT_TIMER, APIC_MAX_OFFSET, APIC_PPR, APIC_SVR, APIC_TDCR, APIC_TMICT, APIC_TPR, Lapic,
    LapicConfig, LapicError,
};
use proptest::prelude::*;

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
        Just(0x030),
        Just(0x2F0),
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

fn drive(ops: &[Op], timer_hz: u64) -> Lapic {
    let mut l = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz,
    })
    .unwrap();
    for op in ops {
        match *op {
            Op::Write { offset, value, now } => {
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

        assert_observationally_equal(&l, &restored)?;

        prop_assert_eq!(restored.snapshot(), snap.clone());

        prop_assert_eq!(l.snapshot(), snap);
    }

    #[test]
    fn snapshot_is_deterministic_across_runs(
        ops in prop::collection::vec(op_strategy(), 1..40),
        timer_hz in 1u64..=4_000_000_000u64,
    ) {
        let a = drive(&ops, timer_hz);
        let b = drive(&ops, timer_hz);
        prop_assert_eq!(a.snapshot(), b.snapshot());
    }

    #[test]
    fn restore_rejects_bad_version(bad in any::<u32>()) {
        let l = Lapic::new(LapicConfig { apic_id: 0, timer_hz: 25_000_000 }).unwrap();
        let mut snap = l.snapshot();
        prop_assume!(bad != snap.version);
        snap.version = bad;
        prop_assert!(Lapic::restore(&snap).is_err());
    }
}

#[test]
fn restore_rejects_inconsistent_timer() {
    let l = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz: 25_000_000,
    })
    .unwrap();

    let mut bad_pending = l.snapshot();
    bad_pending.timer_pending = true;
    bad_pending.initial_count = 0;
    assert!(Lapic::restore(&bad_pending).is_err());

    let mut bad_running = l.snapshot();
    bad_running.timer_running = true;
    bad_running.timer_pending = false;
    bad_running.initial_count = 100;
    assert!(Lapic::restore(&bad_running).is_err());

    let mut bad_disabled = l.snapshot();
    bad_disabled.timer_running = true;
    bad_disabled.timer_pending = true;
    bad_disabled.initial_count = 100;
    assert!(Lapic::restore(&bad_disabled).is_err());

    let mut bad_hz = l.snapshot();
    bad_hz.timer_hz = 0;
    assert!(Lapic::restore(&bad_hz).is_err());

    let mut armed = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz: 25_000_000,
    })
    .unwrap();
    armed
        .mmio_write(lapic::APIC_SVR, 0xFF | (1 << 8), 0)
        .unwrap();
    armed.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
    armed.mmio_write(APIC_TMICT, 1000, 0).unwrap();
    let good = armed.snapshot();
    assert!(good.timer_running && good.timer_pending);
    assert!(Lapic::restore(&good).is_ok());
}

#[test]
fn restore_enforces_anchor_count_bound() {
    let mut armed = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz: 25_000_000,
    })
    .unwrap();
    armed.mmio_write(APIC_SVR, 0xFF | (1 << 8), 0).unwrap();
    armed.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap();
    armed.mmio_write(APIC_TMICT, 1000, 0).unwrap();
    let good = armed.snapshot();
    assert!(good.timer_running && good.timer_pending);

    assert_eq!(good.count_at_arm, good.initial_count);
    assert!(Lapic::restore(&good).is_ok());

    let mut over = good.clone();
    over.count_at_arm = over.initial_count + 1;
    assert_eq!(Lapic::restore(&over).unwrap_err(), LapicError::InvalidState);
}

#[test]
fn ppr_offset_is_exposed() {
    let l = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz: 25_000_000,
    })
    .unwrap();
    assert_eq!(l.mmio_read(APIC_PPR, 0), Ok(0));
}
