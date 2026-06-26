// SPDX-License-Identifier: AGPL-3.0-or-later
//! Systematic register write-mask + restore-validation property tests
//! (PR #38 final systematic pass). Two directions:
//!
//! 1. **Writes never leak reserved bits** — an arbitrary sequence of
//!    `mmio_write`s can never leave any register holding a bit outside its
//!    guest-writable set (read back masked).
//! 2. **`restore` is a real validation boundary** — an arbitrary `LapicState`
//!    is accepted **iff** it is bit-reachable and timer-coherent, otherwise
//!    rejected with `InvalidState`; an accepted state round-trips through
//!    `snapshot` exactly. Cross-checked against an *independent* validator whose
//!    masks are SDM literals (not imported from the crate), so a divergence in
//!    the crate's write-mask table is caught.

use lapic::{
    APIC_DFR, APIC_ESR, APIC_ICR_HIGH, APIC_ICR_LOW, APIC_LDR, APIC_LVT_ERROR, APIC_LVT_LINT0,
    APIC_LVT_LINT1, APIC_LVT_PERFMON, APIC_LVT_THERMAL, APIC_LVT_TIMER, APIC_SVR, APIC_TDCR,
    APIC_TMICT, APIC_TPR, LAPIC_STATE_VERSION, Lapic, LapicConfig, LapicError, LapicState,
};
use proptest::prelude::*;

// --- Independent (SDM-literal) legal-bit masks, NOT imported from the crate ---

const ID_BITS: u32 = 0xFF00_0000;
const TPR_BITS: u32 = 0x0000_00FF;
const SVR_BITS: u32 = 0x0000_13FF;
const LDR_BITS: u32 = 0xFF00_0000;
const DFR_MODEL: u32 = 0xF000_0000;
const DFR_RESERVED_ONES: u32 = 0x0FFF_FFFF;
const ESR_BITS: u32 = 0x0000_0020;
const ICR_LOW_BITS: u32 = 0x000C_CFFF;
const ICR_HIGH_BITS: u32 = 0xFF00_0000;
// Divide-Config: only bits [3,1,0] are stored. Bit 2 is decode-ignored and
// masked off at write time, so a reachable state never holds it.
const TDCR_BITS: u32 = 0x0000_000B;
const SVR_ENABLE: u32 = 1 << 8;
const LVT_MASK_BIT: u32 = 1 << 16;

/// Legal bits per LVT entry (Timer 0, Thermal 1, PerfMon 2, LINT0 3, LINT1 4,
/// Error 5). Error has NO delivery-mode field — only vector + mask.
fn lvt_bits(i: usize) -> u32 {
    match i {
        0 => 0x0007_00FF,     // Timer: vector | mask | timer-mode
        1 | 2 => 0x0001_07FF, // Thermal, PerfMon: vector | delivery-mode | mask
        3 | 4 => 0x0001_A7FF, // LINT0, LINT1: + polarity | trigger
        _ => 0x0001_00FF,     // Error: vector | mask only
    }
}

/// Every register holds only its legal bits (no reserved bit set).
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

/// Independent validator: the full set of invariants a reachable `LapicState`
/// satisfies.
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
    // A running timer's anchor count never exceeds the loaded initial count.
    !(s.timer_running && s.count_at_arm > s.initial_count)
}

/// `restore`'s verdict must match the independent validator, and an accepted
/// state must round-trip through `snapshot` exactly (restore never silently
/// normalizes).
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

// --- Strategies -------------------------------------------------------------

/// A `u32` that is either masked to `valid` bits or fully random.
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

/// An arbitrary `LapicState`, biased so each field is valid ~half the time (so
/// both the accept and reject paths are exercised), but otherwise unconstrained.
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

/// Offsets that hit every writable register (incl. all six LVTs) plus a random
/// aligned offset, so writes land on the registers the masks govern.
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
        (0u32..=0xFF).prop_map(|x| x << 4), // any aligned in-range offset
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Direction 1: no `mmio_write` sequence can leave a reserved bit set in any
    /// register. (Catches the Error-LVT delivery-mode leak.)
    #[test]
    fn mmio_writes_never_set_reserved_bits(
        timer_hz in 1u64..=4_000_000_000u64,
        writes in prop::collection::vec(
            (write_offset(), any::<u32>(), 0u64..=2_000_000_000u64), 0..40),
    ) {
        let mut l = Lapic::new(LapicConfig { apic_id: 0, timer_hz }).unwrap();
        // The fresh state is already canonical.
        prop_assert!(reserved_bits_clear(&l.snapshot()));
        for (offset, value, now) in writes {
            l.mmio_write(offset, value, now).unwrap(); // aligned & in range
            prop_assert!(
                reserved_bits_clear(&l.snapshot()),
                "reserved bit set after write {:#x} = {:#010x}",
                offset, value,
            );
        }
    }

    /// Direction 2: `restore` accepts an arbitrary state iff it is valid, and an
    /// accepted state round-trips exactly. Also a total function (never panics,
    /// only `InvalidState` on rejection).
    #[test]
    fn restore_matches_validator(s in arb_state()) {
        check_restore(&s)?;
    }
}

/// Every register's reserved bits, set one at a time on an otherwise-reachable
/// snapshot, are individually rejected by `restore`.
#[test]
fn restore_rejects_each_reserved_bit() {
    // A reachable, valid base snapshot (enabled, divide set, LVTs written).
    let mut l = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz: 25_000_000,
    })
    .unwrap();
    l.mmio_write(APIC_SVR, 0xFF | SVR_ENABLE, 0).unwrap();
    let base = l.snapshot();
    assert!(Lapic::restore(&base).is_ok());

    // For each register, OR in a reserved bit and expect rejection.
    let corrupt = |mutate: &dyn Fn(&mut LapicState)| {
        let mut s = base.clone();
        mutate(&mut s);
        assert!(
            Lapic::restore(&s).is_err(),
            "restore accepted a state with a reserved bit set"
        );
    };
    corrupt(&|s| s.id |= 0x0000_0001); // ID low bits reserved
    corrupt(&|s| s.tpr |= 0x0000_0100); // TPR > 8 bits
    corrupt(&|s| s.svr |= 0x0000_0400); // SVR bit 10 reserved
    corrupt(&|s| s.ldr |= 0x0000_0001); // LDR low bits reserved
    corrupt(&|s| s.dfr &= 0xFFFF_FFFE); // clear a DFR reserved-one bit
    corrupt(&|s| s.esr |= 0x0000_0001); // ESR bit 0 not modeled
    corrupt(&|s| s.icr_low |= 0x0000_1000); // ICR delivery-status (RO)
    corrupt(&|s| s.icr_high |= 0x0000_0001); // ICR-high low bits reserved
    corrupt(&|s| s.divide_config |= 0x0000_0010); // TDCR bit 4 reserved
    corrupt(&|s| s.divide_config |= 0x0000_0004); // TDCR bit 2 decode-ignored, not stored
    corrupt(&|s| s.lvt[5] |= 0x0000_0100); // Error LVT delivery-mode bit 8 reserved
    corrupt(&|s| s.lvt[0] |= 0x0000_1000); // Timer LVT delivery-status (RO)
}
