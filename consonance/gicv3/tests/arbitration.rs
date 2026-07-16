// SPDX-License-Identifier: AGPL-3.0-or-later
//! Arbitration property test: [`Gicv3::peek_interrupt`] agrees with a naive
//! reference model over arbitrary register-file programs — the gicv3 sibling
//! of `lapic/tests/delivery.rs`.

use gicv3::{GicConfig, GicFrame, Gicv3};
use proptest::prelude::*;

const IMPL_SPIS: u32 = 64; // INTIDs 0..96
const LIMIT: u32 = 32 + IMPL_SPIS;

const SGI_FRAME: u64 = 0x1_0000;
const IGROUPR: u64 = 0x0080;
const ISENABLER: u64 = 0x0100;
const IPRIORITYR: u64 = 0x0400;
const GICD_CTLR: u64 = 0x0000;

/// One programmed interrupt line in the reference model.
#[derive(Clone, Copy, Debug, Default)]
struct Line {
    group1: bool,
    enabled: bool,
    pending: bool,
    active: bool,
    priority: u8,
}

/// The naive reference arbitration: filter, then min by `(priority, intid)`.
fn reference_peek(lines: &[Line], pmr: u8, grp1_enabled: bool) -> Option<u32> {
    if !grp1_enabled {
        return None;
    }
    let running = lines
        .iter()
        .filter(|l| l.active)
        .map(|l| u16::from(l.priority))
        .min()
        .unwrap_or(256);
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.pending && l.enabled && l.group1 && !l.active)
        .filter(|(_, l)| u16::from(l.priority) < u16::from(pmr))
        .filter(|(_, l)| u16::from(l.priority) < running)
        .min_by_key(|(i, l)| (l.priority, *i))
        .map(|(i, _)| i as u32)
}

/// Program `lines` into a fresh model through the real MMIO surface (the
/// distributor for SPIs, the redistributor SGI frame for SGIs/PPIs), raise
/// the pending set, and acknowledge the active set directly.
fn program(lines: &[Line], pmr: u8, grp1_enabled: bool) -> Gicv3 {
    let mut g = Gicv3::new(GicConfig {
        impl_spis: IMPL_SPIS,
        timer_hz: 1_000_000_000,
        timer_intid: 27,
    })
    .unwrap();
    g.mmio_write(
        GicFrame::Dist,
        GICD_CTLR,
        if grp1_enabled { 0b10 } else { 0 },
        0,
    )
    .unwrap();
    g.set_pmr(pmr);
    for (i, l) in lines.iter().enumerate() {
        let intid = i as u32;
        let (w, b) = (intid / 32, intid % 32);
        let (frame, base) = if w == 0 {
            (GicFrame::Redist, SGI_FRAME)
        } else {
            (GicFrame::Dist, 0)
        };
        if l.group1 {
            let off = base + IGROUPR + u64::from(w) * 4;
            let old = g.mmio_read(frame, off, 0).unwrap();
            g.mmio_write(frame, off, old | (1 << b), 0).unwrap();
        }
        if l.enabled {
            g.mmio_write(frame, base + ISENABLER + u64::from(w) * 4, 1 << b, 0)
                .unwrap();
        }
        let off = base + IPRIORITYR + u64::from(intid & !3);
        let shift = 8 * (intid % 4);
        let old = g.mmio_read(frame, off, 0).unwrap();
        g.mmio_write(
            frame,
            off,
            (old & !(0xFF << shift)) | (u32::from(l.priority) << shift),
            0,
        )
        .unwrap();
        if l.pending {
            g.raise(intid).unwrap();
        }
    }
    // Actives last, through the acknowledge path where the reference agrees a
    // take is legal; otherwise via the ISACTIVER bank (a snapshot-shaped
    // state, still architecturally reachable).
    for (i, l) in lines.iter().enumerate() {
        if l.active {
            let intid = i as u32;
            let (w, b) = (intid / 32, intid % 32);
            let (frame, base) = if w == 0 {
                (GicFrame::Redist, SGI_FRAME)
            } else {
                (GicFrame::Dist, 0)
            };
            g.mmio_write(frame, base + 0x0300 + u64::from(w) * 4, 1 << b, 0)
                .unwrap();
        }
    }
    g
}

fn line_strategy() -> impl Strategy<Value = Line> {
    (
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<u8>(),
    )
        .prop_map(|(group1, enabled, pending, active, priority)| Line {
            group1,
            enabled,
            pending,
            active,
            priority,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The model's arbitration equals the reference for every programmed file.
    #[test]
    fn peek_matches_the_reference_model(
        lines in proptest::collection::vec(line_strategy(), LIMIT as usize),
        pmr in any::<u8>(),
        grp1 in any::<bool>(),
    ) {
        let g = program(&lines, pmr, grp1);
        prop_assert_eq!(g.peek_interrupt(), reference_peek(&lines, pmr, grp1));
    }

    /// Acknowledge/EOI walk-down: repeatedly taking the arbitrated INTID and
    /// EOI-ing it drains the deliverable set in strictly non-decreasing
    /// priority order, mirroring the reference at every step.
    #[test]
    fn take_eoi_drains_in_reference_order(
        lines in proptest::collection::vec(line_strategy(), LIMIT as usize),
        pmr in any::<u8>(),
    ) {
        let mut g = program(&lines, pmr, true);
        let mut model = lines.clone();
        let mut last_prio: Option<u8> = None;
        // Bounded: each take clears one pending bit, so LIMIT is a hard cap.
        for _ in 0..LIMIT {
            let expect = reference_peek(&model, pmr, true);
            let got = g.take_interrupt();
            prop_assert_eq!(got, expect);
            let Some(intid) = got else { break };
            let l = &mut model[intid as usize];
            l.pending = false;
            l.active = false; // model take + immediate EOI
            g.eoi(intid).unwrap();
            if let Some(p) = last_prio {
                prop_assert!(l.priority >= p, "priority order violated");
            }
            last_prio = Some(l.priority);
        }
    }

    /// A snapshot round-trip preserves arbitration exactly.
    #[test]
    fn snapshot_preserves_arbitration(
        lines in proptest::collection::vec(line_strategy(), LIMIT as usize),
        pmr in any::<u8>(),
        grp1 in any::<bool>(),
    ) {
        let g = program(&lines, pmr, grp1);
        // The timer is not exercised here (no latch), so any V-time restores.
        let restored = Gicv3::restore(&g.snapshot(), u64::MAX).unwrap();
        prop_assert_eq!(restored.peek_interrupt(), g.peek_interrupt());
        prop_assert_eq!(restored.snapshot(), g.snapshot());
    }
}
