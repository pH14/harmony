// SPDX-License-Identifier: AGPL-3.0-or-later

use lapic::{APIC_IRR, APIC_ISR, APIC_SVR, Lapic, LapicConfig};
use proptest::prelude::*;
use std::collections::BTreeSet;

const SVR_ENABLE: u32 = 1 << 8;

#[derive(Default)]
struct Model {
    pending: BTreeSet<u8>,
    in_service: BTreeSet<u8>,
    tpr: u32,
}

impl Model {
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

fn words(set: &BTreeSet<u8>) -> [u32; 8] {
    let mut w = [0u32; 8];
    for &v in set {
        w[(v >> 5) as usize] |= 1u32 << (v & 31);
    }
    w
}

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

            prop_assert_eq!(l.has_deliverable(), model.has_deliverable());
            prop_assert_eq!(read_words(&l, APIC_IRR), words(&model.pending));
            prop_assert_eq!(read_words(&l, APIC_ISR), words(&model.in_service));
            prop_assert_eq!(l.mmio_read(lapic::APIC_PPR, 0).unwrap(), model.ppr());
        }
    }

    #[test]
    fn lifo_nesting(classes in prop::collection::vec(1u8..=15, 2..8)) {
        let mut l = Lapic::new(LapicConfig { apic_id: 0, timer_hz: 25_000_000 }).unwrap();
        l.mmio_write(APIC_SVR, 0xFF | SVR_ENABLE, 0).unwrap();

        let mut sorted = classes;
        sorted.sort_unstable();
        sorted.dedup();
        let vectors: Vec<u8> = sorted.iter().map(|&c| c << 4).collect();

        for &v in &vectors {
            l.raise(v).unwrap();
            prop_assert_eq!(l.take_interrupt(), Some(v));
        }
        prop_assert!(!l.has_deliverable());

        for &v in vectors.iter().rev() {
            prop_assert_eq!(l.mmio_read(lapic::APIC_PPR, 0).unwrap(), u32::from(v) & 0xF0);
            l.eoi();
        }
        prop_assert_eq!(l.mmio_read(lapic::APIC_PPR, 0).unwrap(), 0);
    }
}
