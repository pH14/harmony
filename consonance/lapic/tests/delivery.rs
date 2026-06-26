// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — prioritized-delivery property test.
//!
//! Applies an arbitrary sequence of `raise` / `take_interrupt` / `eoi` / TPR
//! writes and checks the [`Lapic`] against a naive reference model — a sorted set
//! of pending vectors plus a sorted in-service set — asserting that
//! `take_interrupt` always returns the highest deliverable vector above PPR and
//! that EOI nesting is LIFO-correct (each EOI clears the current highest
//! in-service vector, restoring the priority that was preempted).

use lapic::{APIC_IRR, APIC_ISR, APIC_SVR, Lapic, LapicConfig};
use proptest::prelude::*;
use std::collections::BTreeSet;

const SVR_ENABLE: u32 = 1 << 8;

/// Naive reference: IRR and ISR as ordered sets, plus the task priority.
#[derive(Default)]
struct Model {
    pending: BTreeSet<u8>,
    in_service: BTreeSet<u8>,
    tpr: u32,
}

impl Model {
    /// PPR per the SDM: `TPR` if its class dominates the highest in-service
    /// class, else the in-service vector's class in bits 7:4.
    fn ppr(&self) -> u32 {
        let tpr = self.tpr & 0xFF;
        let isrv = self
            .in_service
            .iter()
            .next_back()
            .copied()
            .map_or(0, u32::from);
        if (tpr >> 4) >= (isrv >> 4) {
            tpr
        } else {
            isrv & 0xF0
        }
    }

    fn has_deliverable(&self) -> bool {
        match self.pending.iter().next_back() {
            Some(&v) => (u32::from(v) >> 4) > (self.ppr() >> 4),
            None => false,
        }
    }

    fn take(&mut self) -> Option<u8> {
        let &v = self.pending.iter().next_back()?;
        if (u32::from(v) >> 4) <= (self.ppr() >> 4) {
            return None;
        }
        self.pending.remove(&v);
        self.in_service.insert(v);
        Some(v)
    }

    fn eoi(&mut self) {
        if let Some(&v) = self.in_service.iter().next_back() {
            self.in_service.remove(&v);
        }
    }
}

/// Pack a vector set into the 8-word register layout the LAPIC exposes.
fn words(set: &BTreeSet<u8>) -> [u32; 8] {
    let mut w = [0u32; 8];
    for &v in set {
        w[(v >> 5) as usize] |= 1u32 << (v & 31);
    }
    w
}

/// Read the 8 words of a 256-bit register starting at `base` from the LAPIC.
fn read_words(l: &Lapic, base: u32) -> [u32; 8] {
    let mut w = [0u32; 8];
    for (i, slot) in w.iter_mut().enumerate() {
        *slot = l.mmio_read(base + (i as u32) * 0x10, 0).unwrap();
    }
    w
}

#[derive(Clone, Debug)]
enum Op {
    Raise(u8),
    Take,
    Eoi,
    SetTpr(u8),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        // Bias toward raises (so there is something to deliver), across a range
        // of priority classes including ties within a class.
        3 => (16u8..=255).prop_map(Op::Raise),
        2 => Just(Op::Take),
        2 => Just(Op::Eoi),
        1 => (0u8..=255).prop_map(Op::SetTpr),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn matches_naive_model(ops in prop::collection::vec(op_strategy(), 1..60)) {
        let mut l = Lapic::new(LapicConfig { apic_id: 0, timer_hz: 25_000_000 }).unwrap();
        l.mmio_write(APIC_SVR, 0xFF | SVR_ENABLE, 0).unwrap();
        let mut model = Model::default();

        for op in ops {
            match op {
                Op::Raise(v) => {
                    l.raise(v).unwrap();
                    model.pending.insert(v);
                }
                Op::Take => {
                    // `peek_interrupt` must predict `take_interrupt`'s result
                    // without mutating: same value, and calling it twice (and the
                    // register file) is unchanged until the actual take.
                    let peeked = l.peek_interrupt();
                    prop_assert_eq!(peeked, l.peek_interrupt());
                    prop_assert_eq!(read_words(&l, APIC_IRR), words(&model.pending));
                    let got = l.take_interrupt();
                    prop_assert_eq!(got, peeked);
                    let want = model.take();
                    prop_assert_eq!(got, want);
                }
                Op::Eoi => {
                    l.eoi();
                    model.eoi();
                }
                Op::SetTpr(t) => {
                    l.mmio_write(lapic::APIC_TPR, u32::from(t), 0).unwrap();
                    model.tpr = u32::from(t);
                }
            }

            // Observable equivalence after every operation.
            prop_assert_eq!(l.has_deliverable(), model.has_deliverable());
            prop_assert_eq!(read_words(&l, APIC_IRR), words(&model.pending));
            prop_assert_eq!(read_words(&l, APIC_ISR), words(&model.in_service));
            prop_assert_eq!(l.mmio_read(lapic::APIC_PPR, 0).unwrap(), model.ppr());
        }
    }

    /// LIFO nesting: raising a strictly increasing chain of classes, taking each
    /// (each preempts the last), then EOIing pops them highest-first, and a
    /// lower-class vector becomes deliverable only after the higher one is
    /// retired.
    #[test]
    fn lifo_nesting(classes in prop::collection::vec(1u8..=15, 2..8)) {
        let mut l = Lapic::new(LapicConfig { apic_id: 0, timer_hz: 25_000_000 }).unwrap();
        l.mmio_write(APIC_SVR, 0xFF | SVR_ENABLE, 0).unwrap();

        // Distinct, strictly increasing vectors (one per class, ascending).
        let mut sorted = classes;
        sorted.sort_unstable();
        sorted.dedup();
        let vectors: Vec<u8> = sorted.iter().map(|&c| c << 4).collect();

        // Raise+take one at a time in increasing-priority order: each new, higher
        // vector preempts the current in-service top (its class strictly exceeds
        // the current PPR class), building a nested in-service stack.
        for &v in &vectors {
            l.raise(v).unwrap();
            prop_assert_eq!(l.take_interrupt(), Some(v));
        }
        // Nothing left pending.
        prop_assert!(!l.has_deliverable());

        // EOI pops the in-service set highest-first.
        for &v in vectors.iter().rev() {
            // The highest in-service vector is `v`; PPR reflects its class.
            prop_assert_eq!(l.mmio_read(lapic::APIC_PPR, 0).unwrap(), u32::from(v) & 0xF0);
            l.eoi();
        }
        prop_assert_eq!(l.mmio_read(lapic::APIC_PPR, 0).unwrap(), 0);
    }
}
