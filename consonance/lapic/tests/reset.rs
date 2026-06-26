// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 5 — reset-state test.
//!
//! [`Lapic::new`] must reproduce the SDM power-on/reset values (Vol. 3A
//! §11.4.7.1): software-disabled APIC, spurious vector `0xFF`, every LVT masked,
//! all counts and priority registers zero — and **no interrupt is deliverable
//! until the guest software-enables the APIC**.

use lapic::{
    APIC_DFR, APIC_ID, APIC_IRR, APIC_ISR, APIC_LVT_ERROR, APIC_LVT_LINT0, APIC_LVT_LINT1,
    APIC_LVT_PERFMON, APIC_LVT_THERMAL, APIC_LVT_TIMER, APIC_PPR, APIC_SVR, APIC_TMCCT, APIC_TMICT,
    APIC_TMR, APIC_TPR, APIC_VERSION, APIC_VERSION_VALUE, Lapic, LapicConfig,
};

const SVR_ENABLE: u32 = 1 << 8;
const LVT_MASK: u32 = 1 << 16;

fn reset_lapic() -> Lapic {
    Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz: 25_000_000,
    })
    .expect("non-zero timer_hz")
}

#[test]
fn apic_is_software_disabled_at_reset() {
    let l = reset_lapic();
    let svr = l.mmio_read(APIC_SVR, 0).unwrap();
    assert_eq!(
        svr & SVR_ENABLE,
        0,
        "SVR bit 8 (software enable) must be clear"
    );
    assert_eq!(
        svr, 0x0000_00FF,
        "reset SVR is spurious vector 0xFF, disabled"
    );
}

#[test]
fn all_lvt_entries_masked_at_reset() {
    let l = reset_lapic();
    for off in [
        APIC_LVT_TIMER,
        APIC_LVT_THERMAL,
        APIC_LVT_PERFMON,
        APIC_LVT_LINT0,
        APIC_LVT_LINT1,
        APIC_LVT_ERROR,
    ] {
        let lvt = l.mmio_read(off, 0).unwrap();
        assert_eq!(
            lvt & LVT_MASK,
            LVT_MASK,
            "LVT {off:#x} must be masked at reset"
        );
    }
}

#[test]
fn counts_and_priorities_zero_at_reset() {
    let l = reset_lapic();
    assert_eq!(l.mmio_read(APIC_TPR, 0), Ok(0));
    assert_eq!(l.mmio_read(APIC_PPR, 0), Ok(0));
    assert_eq!(l.mmio_read(APIC_TMICT, 0), Ok(0));
    assert_eq!(l.mmio_read(APIC_TMCCT, 12_345), Ok(0));
    // ISR / IRR / TMR all clear (sweep the 8 words of each).
    for base in [APIC_ISR, APIC_IRR, APIC_TMR] {
        for word in 0..8u32 {
            assert_eq!(l.mmio_read(base + word * 0x10, 0), Ok(0));
        }
    }
}

#[test]
fn id_and_version_at_reset() {
    let l = Lapic::new(LapicConfig {
        apic_id: 0,
        timer_hz: 25_000_000,
    })
    .unwrap();
    assert_eq!(l.mmio_read(APIC_ID, 0), Ok(0));
    assert_eq!(l.mmio_read(APIC_VERSION, 0), Ok(APIC_VERSION_VALUE));
    // DFR resets to the flat-model all-ones value.
    assert_eq!(l.mmio_read(APIC_DFR, 0), Ok(0xFFFF_FFFF));
}

#[test]
fn nothing_deliverable_until_enabled() {
    let mut l = reset_lapic();
    // A raised interrupt is held but not deliverable while software-disabled.
    l.raise(0x40).unwrap();
    assert!(!l.has_deliverable(), "disabled APIC delivers nothing");
    assert_eq!(l.take_interrupt(), None);

    // The timer cannot fire while disabled, either.
    l.mmio_write(APIC_LVT_TIMER, 0x40, 0).unwrap(); // unmasked one-shot, vector 0x40
    l.mmio_write(lapic::APIC_TMICT, 100, 0).unwrap();
    assert_eq!(l.next_timer_deadline(), None);

    // Software-enable the APIC: the already-pending vector becomes deliverable.
    l.mmio_write(APIC_SVR, 0xFF | SVR_ENABLE, 0).unwrap();
    assert!(l.has_deliverable());
    assert_eq!(l.take_interrupt(), Some(0x40));
}

#[test]
fn timer_stopped_at_reset() {
    let mut l = reset_lapic();
    assert_eq!(l.next_timer_deadline(), None);
    assert!(!l.advance_to(u64::MAX), "a stopped timer never fires");
}
