// SPDX-License-Identifier: AGPL-3.0-or-later

use lapic::{
    APIC_DFR, APIC_ESR, APIC_ICR_HIGH, APIC_ICR_LOW, APIC_LDR, APIC_LVT_ERROR, APIC_LVT_LINT0,
    APIC_LVT_LINT1, APIC_LVT_PERFMON, APIC_LVT_THERMAL, APIC_LVT_TIMER, APIC_SVR, APIC_TDCR,
    APIC_TMICT, APIC_TPR, LAPIC_STATE_VERSION, Lapic, LapicConfig, LapicError, LapicState,
};
use proptest::prelude::*;

const ID_BITS: u32 = 0xFF00_0000;
const TPR_BITS: u32 = 0x0000_00FF;
const SVR_BITS: u32 = 0x0000_13FF;
const LDR_BITS: u32 = 0xFF00_0000;
const DFR_MODEL: u32 = 0xF000_0000;
const DFR_RESERVED_ONES: u32 = 0x0FFF_FFFF;
const ESR_BITS: u32 = 0x0000_0020;
const ICR_LOW_BITS: u32 = 0x000C_CFFF;
const ICR_HIGH_BITS: u32 = 0xFF00_0000;
const TDCR_BITS: u32 = 0x0000_000B;
const SVR_ENABLE: u32 = 1 << 8;
const LVT_MASK_BIT: u32 = 1 << 16;

fn lvt_bits(i: usize) -> u32 {
    match i {
        0 => 0x0007_00FF,
        1 | 2 => 0x0001_07FF,
        3 | 4 => 0x0001_A7FF,
        _ => 0x0001_00FF,
    }
}

fn reserved_bits_clear(s: &LapicState) -> bool {
    let regs = s.id & !ID_BITS == 0
        && s.tpr & !TPR_BITS == 0
        && s.svr & !SVR_BITS == 0
        && s.ldr & !LDR_BITS == 0
        && s.dfr & DFR_RESERVED_ONES == DFR_RESERVED_ONES
        && s.esr & !ESR_BITS == 0
        && s.icr_low & !ICR_LOW_BITS == 0
        && s.icr_high & !ICR_HIGH_BITS == 0
        && s.divide_config & !TDCR_BITS == 0;
    regs && (0..6).all(|i| s.lvt[i] & !lvt_bits(i) == 0)
}

fn expected_valid(s: &LapicState) -> bool {
    if s.version != LAPIC_STATE_VERSION || s.timer_hz == 0 {
        return false;
    }
    if !reserved_bits_clear(s) {
        return false;
    }
    if s.timer_pending && s.initial_count == 0 {
        return false;
    }
    let armable = s.timer_pending
        && s.svr & SVR_ENABLE != 0
        && s.lvt[0] & LVT_MASK_BIT == 0
        && matches!((s.lvt[0] >> 17) & 0b11, 0 | 1);
    if s.timer_running != armable {
        return false;
    }
    !(s.timer_running && s.count_at_arm > s.initial_count)
}

fn check_restore(s: &LapicState) -> Result<(), TestCaseError> {
    let valid = expected_valid(s);
    match Lapic::restore(s) {
        Ok(l) => {
            prop_assert!(valid, "restore accepted an unreachable state");
            prop_assert_eq!(&l.snapshot(), s, "restore must round-trip exactly");
        }
        Err(LapicError::InvalidState) => {
            prop_assert!(!valid, "restore rejected a reachable state");
        }
        Err(e) => prop_assert!(false, "unexpected error {:?}", e),
    }
    Ok(())
}

fn biased(valid: u32) -> impl Strategy<Value = u32> {
    prop_oneof![any::<u32>().prop_map(move |v| v & valid), any::<u32>()]
}

fn biased_dfr() -> impl Strategy<Value = u32> {
    prop_oneof![
        any::<u32>().prop_map(|v| (v & DFR_MODEL) | DFR_RESERVED_ONES),
        any::<u32>(),
    ]
}

fn arb_lvt() -> impl Strategy<Value = [u32; 6]> {
    (
        biased(lvt_bits(0)),
        biased(lvt_bits(1)),
        biased(lvt_bits(2)),
        biased(lvt_bits(3)),
        biased(lvt_bits(4)),
        biased(lvt_bits(5)),
    )
        .prop_map(|(a, b, c, d, e, f)| [a, b, c, d, e, f])
}

fn arb_state() -> impl Strategy<Value = LapicState> {
    let head = (
        prop_oneof![Just(LAPIC_STATE_VERSION), any::<u32>()],
        biased(ID_BITS),
        prop_oneof![Just(25_000_000u64), Just(0u64), any::<u64>()],
        biased(TPR_BITS),
        biased(SVR_BITS),
        biased(LDR_BITS),
        biased_dfr(),
        biased(ESR_BITS),
        biased(ICR_LOW_BITS),
        biased(ICR_HIGH_BITS),
    );
    let tail = (
        biased(TDCR_BITS),
        any::<[u32; 8]>(),
        any::<[u32; 8]>(),
        any::<[u32; 8]>(),
        arb_lvt(),
        any::<u32>(),
        any::<u32>(),
        any::<u64>(),
        any::<bool>(),
        any::<bool>(),
    );
    (head, tail).prop_map(
        |(
            (version, id, timer_hz, tpr, svr, ldr, dfr, esr, icr_low, icr_high),
            (
                divide_config,
                isr,
                tmr,
                irr,
                lvt,
                initial_count,
                count_at_arm,
                timer_arm_vns,
                timer_running,
                timer_pending,
            ),
        )| LapicState {
            version,
            id,
            timer_hz,
            tpr,
            svr,
            ldr,
            dfr,
            esr,
            icr_low,
            icr_high,
            divide_config,
            isr,
            tmr,
            irr,
            lvt,
            initial_count,
            count_at_arm,
            timer_arm_vns,
            timer_running,
            timer_pending,
        },
    )
}

fn write_offset() -> impl Strategy<Value = u32> {
    prop_oneof![
        Just(APIC_TPR),
        Just(APIC_LDR),
        Just(APIC_DFR),
        Just(APIC_SVR),
        Just(APIC_ESR),
        Just(APIC_ICR_LOW),
        Just(APIC_ICR_HIGH),
        Just(APIC_TDCR),
        Just(APIC_TMICT),
        Just(APIC_LVT_TIMER),
        Just(APIC_LVT_THERMAL),
        Just(APIC_LVT_PERFMON),
        Just(APIC_LVT_LINT0),
        Just(APIC_LVT_LINT1),
        Just(APIC_LVT_ERROR),
        (0u32..=0xFF).prop_map(|x| x << 4),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn mmio_writes_never_set_reserved_bits(
        timer_hz in 1u64..=4_000_000_000u64,
        writes in prop::collection::vec(
            (write_offset(), any::<u32>(), 0u64..=2_000_000_000u64), 0..40),
    ) {
        let mut l = Lapic::new(LapicConfig { apic_id: 0, timer_hz }).unwrap();
        prop_assert!(reserved_bits_clear(&l.snapshot()));
        for (offset, value, now) in writes {
            l.mmio_write(offset, value, now).unwrap();
            prop_assert!(
                reserved_bits_clear(&l.snapshot()),
                "reserved bit set after write {:#x} = {:#010x}",
                offset, value,
            );
        }
    }

    #[test]
    fn restore_matches_validator(s in arb_state()) {
        check_restore(&s)?;
    }
}

#[test]
fn restore_rejects_each_reserved_bit() {
    let mut l = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz: 25_000_000,
    })
    .unwrap();
    l.mmio_write(APIC_SVR, 0xFF | SVR_ENABLE, 0).unwrap();
    let base = l.snapshot();
    assert!(Lapic::restore(&base).is_ok());

    let corrupt = |mutate: &dyn Fn(&mut LapicState)| {
        let mut s = base.clone();
        mutate(&mut s);
        assert!(
            Lapic::restore(&s).is_err(),
            "restore accepted a state with a reserved bit set"
        );
    };
    corrupt(&|s| s.id |= 0x0000_0001);
    corrupt(&|s| s.tpr |= 0x0000_0100);
    corrupt(&|s| s.svr |= 0x0000_0400);
    corrupt(&|s| s.ldr |= 0x0000_0001);
    corrupt(&|s| s.dfr &= 0xFFFF_FFFE);
    corrupt(&|s| s.esr |= 0x0000_0001);
    corrupt(&|s| s.icr_low |= 0x0000_1000);
    corrupt(&|s| s.icr_high |= 0x0000_0001);
    corrupt(&|s| s.divide_config |= 0x0000_0010);
    corrupt(&|s| s.divide_config |= 0x0000_0004);
    corrupt(&|s| s.lvt[5] |= 0x0000_0100);
    corrupt(&|s| s.lvt[0] |= 0x0000_1000);
}
