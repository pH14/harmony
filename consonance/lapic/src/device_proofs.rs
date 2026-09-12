// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const EXACT_BOUND: u64 = (1 << 12) - 1;

const PROOF_TIMER_HZ: u64 = 25_000_000;

const DIVIDE_CONFIGS: [u32; 8] = [
    0b0000, 0b0001, 0b0010, 0b0011, 0b1000, 0b1001, 0b1010, 0b1011,
];

fn timer_lapic(timer_hz: u64, divide_config: u32, initial_count: u32, arm_vns: u64) -> Lapic {
    Lapic {
        id: 0,
        timer_hz,
        tpr: 0,
        svr: 0,
        ldr: 0,
        dfr: 0,
        esr: 0,
        icr_low: 0,
        icr_high: 0,
        divide_config,
        isr: [0; 8],
        tmr: [0; 8],
        irr: [0; 8],
        lvt: [LVT_RESET; 6],
        initial_count,
        count_at_arm: initial_count,
        timer_arm_vns: arm_vns,
        timer_running: true,
        timer_pending: true,
    }
}

#[kani::proof]
fn divide_value_total() {
    let cfg: u32 = kani::any();
    let d = divide_value(cfg);
    assert!(matches!(d, 1 | 2 | 4 | 8 | 16 | 32 | 64 | 128));
}

#[kani::proof]
fn period_never_panics_any_count() {
    let n: u32 = kani::any();
    let divide_config: u32 = kani::any();
    let l = timer_lapic(1, divide_config, n, 0);
    let _ = l.period_for(l.count_at_arm);
}

#[kani::proof]
#[kani::unwind(4)]
fn huge_period_reports_no_deadline() {
    for n in [2_000_000_000u32, 3_000_000_000, u32::MAX] {
        let l = running_periodic_lapic(1, 0b1010, n, 0);
        assert!(l.period_for(l.count_at_arm) > u128::from(u64::MAX));
        assert_eq!(l.next_timer_deadline(), None);
    }
}

const REP_DIVIDE_CONFIG: u32 = 0b0011;

#[kani::proof]
fn period_exact_ceil() {
    let n: u32 = kani::any();
    kani::assume(u64::from(n) <= EXACT_BOUND);

    let l = timer_lapic(PROOF_TIMER_HZ, REP_DIVIDE_CONFIG, n, 0);
    let got = l.period_for(l.count_at_arm);

    let divide = divide_value(REP_DIVIDE_CONFIG);
    let numer = u128::from(n) * u128::from(divide) * NS_PER_SEC;
    assert_eq!(got, numer.div_ceil(u128::from(PROOF_TIMER_HZ)));
    assert!(got * u128::from(PROOF_TIMER_HZ) >= numer);
}

#[kani::proof]
#[kani::unwind(9)]
fn current_count_round_trips_at_arm() {
    let n: u32 = kani::any();
    let arm: u64 = kani::any();
    for divide_config in DIVIDE_CONFIGS {
        let l = timer_lapic(PROOF_TIMER_HZ, divide_config, n, arm);
        assert_eq!(l.current_count(arm), n);
    }
}

#[kani::proof]
fn current_count_exact_decay() {
    let n: u32 = kani::any();
    let delta: u64 = kani::any();
    kani::assume(u64::from(n) <= EXACT_BOUND);
    kani::assume(delta <= EXACT_BOUND);
    let arm: u64 = kani::any();
    kani::assume(arm <= EXACT_BOUND);
    let now = arm + delta;

    let l = timer_lapic(PROOF_TIMER_HZ, REP_DIVIDE_CONFIG, n, arm);
    let got = l.current_count(now);

    let divide = divide_value(REP_DIVIDE_CONFIG);
    let ticks =
        (u128::from(delta) * u128::from(PROOF_TIMER_HZ)) / (u128::from(divide) * NS_PER_SEC);
    let want = n.saturating_sub(u32::try_from(ticks).unwrap_or(u32::MAX));
    assert_eq!(got, want);
}

#[kani::proof]
fn current_count_monotone() {
    let n: u32 = kani::any();
    let arm: u64 = kani::any();
    let d1: u64 = kani::any();
    let d2: u64 = kani::any();
    kani::assume(d1 <= d2);
    kani::assume(arm <= EXACT_BOUND && d2 <= EXACT_BOUND);
    let l = timer_lapic(PROOF_TIMER_HZ, 0b0000, n, arm);

    assert!(l.current_count(arm + d1) >= l.current_count(arm + d2));
}

#[kani::proof]
fn vec_index_in_bounds() {
    let v: u8 = kani::any();
    let mut bits = [0u32; 8];
    set_vec(&mut bits, v);
    assert_eq!(highest_vec(&bits), Some(v));
    clear_vec(&mut bits, v);
    assert!(highest_vec(&bits).is_none());
}

#[kani::proof]
#[kani::unwind(9)]
fn highest_vec_correct() {
    let bits: [u32; 8] = kani::any();
    match highest_vec(&bits) {
        None => {
            let mut any_set = false;
            for w in 0..8 {
                if bits[w] != 0 {
                    any_set = true;
                }
            }
            assert!(!any_set);
        }
        Some(v) => {
            assert!(bits[(v >> 5) as usize] & (1u32 << (v & 31)) != 0);
            assert!(priority_class(v) <= 15);
        }
    }
}

#[kani::proof]
fn ppr_and_delivery_total() {
    let tpr: u32 = kani::any();
    let isr: [u32; 8] = kani::any();
    let irr: [u32; 8] = kani::any();
    let mut l = timer_lapic(PROOF_TIMER_HZ, 0, 0, 0);
    l.svr = SVR_ENABLE_BIT;
    l.tpr = tpr;
    l.isr = isr;
    l.irr = irr;

    assert!((l.ppr() >> 4) <= 15);

    let deliverable = l.has_deliverable();
    let taken = l.take_interrupt();
    assert_eq!(deliverable, taken.is_some());
}

fn running_periodic_lapic(
    timer_hz: u64,
    divide_config: u32,
    initial_count: u32,
    arm_vns: u64,
) -> Lapic {
    let mut lvt = [LVT_RESET; 6];
    lvt[LVT_TIMER] = 0x40 | (TIMER_PERIODIC << 17);
    Lapic {
        id: 0,
        timer_hz,
        tpr: 0,
        svr: SVR_ENABLE_BIT,
        ldr: 0,
        dfr: 0,
        esr: 0,
        icr_low: 0,
        icr_high: 0,
        divide_config,
        isr: [0; 8],
        tmr: [0; 8],
        irr: [0; 8],
        lvt,
        initial_count,
        count_at_arm: initial_count,
        timer_arm_vns: arm_vns,
        timer_running: true,
        timer_pending: true,
    }
}

#[kani::proof]
fn advance_to_idempotent_at_saturation_boundary() {
    let arm: u64 = kani::any();
    kani::assume(arm >= u64::MAX - 240_000_000);
    let now = u64::MAX;
    let mut l = running_periodic_lapic(PROOF_TIMER_HZ, 0b0000, 1_000_000, arm);

    let _ = l.advance_to(now);
    let arm1 = l.timer_arm_vns;
    let irr1 = l.irr;
    let running1 = l.timer_running;

    let second = l.advance_to(now);
    assert!(!second);
    assert_eq!(l.timer_arm_vns, arm1);
    assert_eq!(l.irr, irr1);
    assert_eq!(l.timer_running, running1);
}

#[kani::proof]
fn tdcr_change_no_retroactive_fire() {
    let mut lvt = [LVT_RESET; 6];
    lvt[LVT_TIMER] = 0x40;
    let mut l = Lapic {
        id: 0,
        timer_hz: PROOF_TIMER_HZ,
        tpr: 0,
        svr: SVR_ENABLE_BIT,
        ldr: 0,
        dfr: 0,
        esr: 0,
        icr_low: 0,
        icr_high: 0,
        divide_config: 0b0000,
        isr: [0; 8],
        tmr: [0; 8],
        irr: [0; 8],
        lvt,
        initial_count: 1000,
        count_at_arm: 1000,
        timer_arm_vns: 0,
        timer_running: true,
        timer_pending: true,
    };
    let now: u64 = kani::any();
    kani::assume(now < 80_000);
    let remaining_before = l.current_count(now);
    kani::assume(remaining_before > 0);

    l.mmio_write(APIC_TDCR, 0b1010, now).unwrap();

    assert_eq!(l.current_count(now), remaining_before);
    assert!(!l.advance_to(now));
    if let Some(d) = l.next_timer_deadline() {
        assert!(d > now);
    }
}

#[kani::proof]
fn fired_oneshot_not_resurrected() {
    let n: u32 = kani::any();
    kani::assume(n != 0);
    let now: u64 = kani::any();

    let mut lvt = [LVT_RESET; 6];
    lvt[LVT_TIMER] = 0x40;
    let mut l = Lapic {
        id: 0,
        timer_hz: PROOF_TIMER_HZ,
        tpr: 0,
        svr: SVR_ENABLE_BIT,
        ldr: 0,
        dfr: 0,
        esr: 0,
        icr_low: 0,
        icr_high: 0,
        divide_config: 0,
        isr: [0; 8],
        tmr: [0; 8],
        irr: [0; 8],
        lvt,
        initial_count: n,
        count_at_arm: n,
        timer_arm_vns: 0,
        timer_running: false,
        timer_pending: false,
    };

    let prior = l.running_remaining(now);
    l.retime(now, prior, divide_value(l.divide_config));
    assert!(!l.timer_running);
    assert_eq!(l.next_timer_deadline(), None);
    assert!(!l.advance_to(u64::MAX));
}

#[kani::proof]
fn tdcr_write_mask_drops_ignored_bit() {
    let value: u32 = kani::any();
    let stored = value & TDCR_WRITE_MASK;
    assert_eq!(stored & 0b100, 0);
    assert_eq!(stored & !0b1011, 0);
    assert_eq!(divide_value(stored), divide_value(value));
}

#[kani::proof]
#[kani::unwind(7)]
fn lvt_write_masks_exclude_reserved() {
    let value: u32 = kani::any();
    assert_eq!(value & lvt_write_mask(5) & 0x0000_0700, 0);
    for i in 0..6 {
        let stored = value & lvt_write_mask(i);
        assert_eq!(stored & (1 << 12), 0);
        assert_eq!(stored & (1 << 14), 0);
        assert_eq!(stored & !lvt_write_mask(i), 0);
    }
}
