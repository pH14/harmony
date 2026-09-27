// SPDX-License-Identifier: AGPL-3.0-or-later

extern crate std;

use super::*;

const IDLE_PRIORITY: u16 = 256;

impl Gicv3 {
    fn baseline_running_priority(&self) -> u16 {
        let mut best = IDLE_PRIORITY;
        for w in 0..BITMAP_WORDS {
            let mut bits = self.active[w];
            while bits != 0 {
                let bit = bits.trailing_zeros();
                bits &= bits - 1;
                let intid = (w as u32) * 32 + bit;
                if self.implemented(intid) {
                    best = best.min(u16::from(self.priority[intid as usize]));
                }
            }
        }
        best
    }
    fn baseline_peek_interrupt(&self) -> Option<u32> {
        if self.gicd_ctlr & GICD_CTLR_ENABLE_GRP1 == 0 || !self.igrpen1 {
            return None;
        }
        let running = self.baseline_running_priority();
        let pmr = u16::from(self.pmr);
        let mut best: Option<(u16, u32)> = None;
        for w in 0..BITMAP_WORDS {
            let mut bits = (self.pending[w] | self.line_level[w])
                & self.enable[w]
                & self.group[w]
                & !self.active[w];
            while bits != 0 {
                let bit = bits.trailing_zeros();
                bits &= bits - 1;
                let intid = (w as u32) * 32 + bit;
                if !self.implemented(intid) {
                    continue;
                }
                let prio = u16::from(self.priority[intid as usize]);
                if prio >= pmr || prio >= running {
                    continue;
                }
                let key = (prio, intid);
                if best.is_none_or(|b| key < b) {
                    best = Some(key);
                }
            }
        }
        best.map(|(_, intid)| intid)
    }
    fn baseline_active_interrupt(&self) -> Option<u32> {
        let mut best: Option<(u16, u32)> = None;
        for word in 0..BITMAP_WORDS {
            for bit in 0..32 {
                if self.active[word] & (1 << bit) == 0 {
                    continue;
                }
                let intid = word as u32 * 32 + bit;
                if !self.implemented(intid) {
                    continue;
                }
                let key = (u16::from(self.priority[intid as usize]), intid);
                if best.is_none_or(|current| key.cmp(&current).is_lt()) {
                    best = Some(key);
                }
            }
        }
        best.map(|(_, intid)| intid)
    }
}

fn populated(spis: u32, stride: usize) -> Gicv3 {
    let mut g = Gicv3::new(GicConfig {
        impl_spis: spis,
        timer_hz: 1_000_000_000,
        timer_intid: 27,
    })
    .unwrap();
    g.gicd_ctlr = GICD_CTLR_ENABLE_GRP1;
    g.igrpen1 = true;
    g.pmr = 255;
    for i in 0..g.intid_limit() as usize {
        g.priority[i] = ((i * 67 + 13) % 255) as u8;
        g.enable[i / 32] |= 1 << (i % 32);
        g.pending[i / 32] |= 1 << (i % 32);
        if stride != 0 && i % stride == 0 {
            g.active[i / 32] |= 1 << (i % 32);
        }
    }
    g
}

#[test]
fn arbitration_matches_full_scan_across_implemented_ranges() {
    for spis in (0..=960).step_by(32) {
        for stride in [0, 1, 2, 7, 32, 93, 991] {
            let mut g = populated(spis, stride);
            for pmr in [0, 1, 42, 128, 255] {
                g.set_pmr(pmr);
                for priority in [0, 1, 42, 128, 255] {
                    assert_eq!(
                        g.preempts_active(priority),
                        priority < g.baseline_running_priority()
                    );
                }
                assert_eq!(g.active_interrupt(), g.baseline_active_interrupt());
                assert_eq!(g.peek_interrupt(), g.baseline_peek_interrupt());
                let before = g.snapshot();
                let expected = g.baseline_peek_interrupt();
                let taken = g.take_interrupt();
                assert_eq!(taken, expected);
                if let Some(id) = taken {
                    g.eoi(id).unwrap();
                }
                g = Gicv3::restore(&before, 0).unwrap();
                assert_eq!(g.snapshot(), before);
            }
        }
    }
}

fn workload(spis: u32, name: &str) -> Gicv3 {
    let mut g = populated(spis, 0);
    let limit = g.intid_limit() as usize;
    match name {
        "idle" => g.pending.fill(0),
        "timer" => {
            g.pending.fill(0);
            g.pending[0] = 1 << 27;
        }
        "handler" => {
            g.pending.fill(0);
            g.active[0] = 1 << 27;
        }
        "nested" => {
            g.pending.fill(0);
            g.active[0] = 1 << 27;
            g.pending[0] = 1 << 28;
            g.priority[27] = 128;
            g.priority[28] = 64;
        }
        "dense-active" => {
            g.active = g.pending;
            for priority in &mut g.priority[..limit] {
                *priority = (*priority).max(1);
            }
        }
        "dense-masked" | "tied-active" => {
            for word in &mut g.active[..limit / 32] {
                *word = 0xaaaa_aaaa;
            }
            g.priority[..limit].fill(128);
            if name == "dense-masked" {
                g.pmr = 128;
            }
        }
        "last-winner" => {
            g.priority[..limit].fill(200);
            g.priority[limit - 1] = 0;
        }
        "equal-pending" => g.priority[..limit].fill(128),
        "preempt-dense" => {
            for word in &mut g.active[..limit / 32] {
                *word = 0xaaaa_aaaa;
            }
            for (i, priority) in g.priority[..limit].iter_mut().enumerate() {
                *priority = if i % 2 == 0 { 64 } else { 128 };
            }
        }
        "last-active" => {
            g.pending.fill(0);
            g.pending[0] = 1 << 27;
            g.priority[27] = 64;
            g.active[(limit - 1) / 32] = 1 << ((limit - 1) % 32);
            g.priority[limit - 1] = 128;
        }
        "level-only" => {
            g.pending.fill(0);
            g.line_level[(limit - 1) / 32] = 1 << ((limit - 1) % 32);
        }
        "pmr-zero" => g.pmr = 0,
        "dense-pending" => {}
        _ => panic!("unknown workload"),
    }
    g
}

#[test]
#[ignore = "same-executable native timing qualification"]
fn qualify_arbitration() {
    use std::hint::black_box;
    for spis in [0, 64, 960] {
        for name in [
            "idle",
            "timer",
            "handler",
            "nested",
            "dense-active",
            "dense-masked",
            "tied-active",
            "last-winner",
            "equal-pending",
            "dense-pending",
            "preempt-dense",
            "last-active",
            "level-only",
            "pmr-zero",
        ] {
            let g = workload(spis, name);
            for operation in ["active", "peek"] {
                let control = if operation == "active" {
                    Gicv3::baseline_active_interrupt
                } else {
                    Gicv3::baseline_peek_interrupt
                };
                let proposed = if operation == "active" {
                    Gicv3::active_interrupt
                } else {
                    Gicv3::peek_interrupt
                };
                assert_eq!(control(&g), proposed(&g));
                let before = g.snapshot();
                for pair in 0..9 {
                    for optimized in if pair % 2 == 0 {
                        [false, true]
                    } else {
                        [true, false]
                    } {
                        let query = black_box(if optimized { proposed } else { control });
                        #[allow(clippy::disallowed_methods)]
                        let start = std::time::Instant::now();
                        for _ in 0..50_000 {
                            black_box(query(black_box(&g)));
                        }
                        let ns = start.elapsed().as_nanos() as f64 / 50_000.0;
                        std::println!("gic,{spis},{name},{operation},{pair},{optimized},{ns}");
                    }
                }
                assert_eq!(g.snapshot(), before);
            }
        }
    }
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(256))]
    #[test]
    fn randomized_arbitration_and_acceptance_match_full_scan(
        words in proptest::collection::vec(
            (proptest::prelude::any::<u32>(), proptest::prelude::any::<u32>(),
             proptest::prelude::any::<u32>(), proptest::prelude::any::<u32>(),
             proptest::prelude::any::<u32>(), proptest::array::uniform32(proptest::prelude::any::<u8>())), 1..=31),
        pmr in proptest::prelude::any::<u8>(),
        group1 in proptest::prelude::any::<bool>(),
        distributor in proptest::prelude::any::<bool>(),
    ) {
        let mut g = populated((words.len() as u32 - 1) * 32, 0);
        g.pmr = pmr;
        g.igrpen1 = group1;
        g.gicd_ctlr = if distributor { GICD_CTLR_ENABLE_GRP1 } else { 0 };
        for (w, &(group, enable, pending, active, level, priority)) in words.iter().enumerate() {
            g.group[w] = group;
            g.enable[w] = enable;
            g.pending[w] = pending;
            g.active[w] = active;
            g.line_level[w] = level;
            g.priority[w * 32..(w + 1) * 32].copy_from_slice(&priority);
        }
        let mut control = Gicv3::restore(&g.snapshot(), 0).unwrap();
        for _ in 0..16 {
            proptest::prop_assert_eq!(g.active_interrupt(), control.baseline_active_interrupt());
            let expected = control.baseline_peek_interrupt();
            proptest::prop_assert_eq!(g.peek_interrupt(), expected);
            proptest::prop_assert_eq!(g.take_interrupt(), expected);
            if let Some(id) = expected {
                control.pending[id as usize / 32] &= !(1 << (id % 32));
                control.active[id as usize / 32] |= 1 << (id % 32);
            }
            proptest::prop_assert_eq!(g.snapshot(), control.snapshot());
            if let Some(id) = control.baseline_active_interrupt() {
                g.eoi(id).unwrap();
                control.active[id as usize / 32] &= !(1 << (id % 32));
            }
            proptest::prop_assert_eq!(g.snapshot(), control.snapshot());
        }
    }
}

fn delivery_cycle(g: &mut Gicv3, baseline: bool) -> [Option<u32>; 3] {
    g.pulse(27).unwrap();
    let pending = if baseline {
        g.baseline_peek_interrupt()
    } else {
        g.peek_interrupt()
    };
    let taken = if baseline {
        let id = g.baseline_peek_interrupt();
        if let Some(id) = id {
            g.pending[id as usize / 32] &= !(1 << (id % 32));
            g.active[id as usize / 32] |= 1 << (id % 32);
        }
        id
    } else {
        g.take_interrupt()
    };
    let active = if baseline {
        g.baseline_active_interrupt()
    } else {
        g.active_interrupt()
    };
    g.eoi(27).unwrap();
    [pending, taken, active]
}

#[test]
fn delivery_cycles_match_full_scan_through_restore() {
    let mut proposed = workload(64, "idle");
    let mut control = workload(64, "idle");
    for cycle in 0..64 {
        assert_eq!(
            delivery_cycle(&mut proposed, false),
            delivery_cycle(&mut control, true)
        );
        assert_eq!(proposed.snapshot(), control.snapshot());
        if cycle == 31 {
            proposed = Gicv3::restore(&proposed.snapshot(), 0).unwrap();
            control = Gicv3::restore(&control.snapshot(), 0).unwrap();
        }
    }
}

#[test]
#[ignore = "same-executable native timing qualification"]
fn qualify_delivery_cycle() {
    use std::hint::black_box;
    for pair in 0..9 {
        let mut endpoints = std::vec::Vec::new();
        for baseline in if pair % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let mut g = workload(64, "idle");
            let mut transcript = std::vec![[None; 3]; 50_000];
            #[allow(clippy::disallowed_methods)]
            let start = std::time::Instant::now();
            for outputs in &mut transcript {
                *outputs = delivery_cycle(black_box(&mut g), black_box(baseline));
            }
            let ns = start.elapsed().as_nanos() as f64 / transcript.len() as f64;
            assert!(transcript.iter().all(|v| *v == [Some(27); 3]));
            endpoints.push((transcript, g.snapshot()));
            std::println!("cycle,{pair},{baseline},{ns}");
        }
        assert_eq!(endpoints[0], endpoints[1]);
    }
}

#[test]
fn minimum_candidate_still_checks_active_blockers_and_lowest_intid_ties() {
    let mut g = populated(960, 0);
    g.priority[..992].fill(200);
    g.priority[3] = 0;
    g.priority[991] = 0;
    g.priority[900] = 0;
    g.active[900 / 32] = 1 << (900 % 32);
    let before = g.snapshot();
    assert_eq!(g.peek_interrupt(), None);
    assert_eq!(g.take_interrupt(), None);
    assert_eq!(g.snapshot(), before);
    g.priority[900] = 1;
    assert_eq!(g.take_interrupt(), Some(3));
    assert_eq!(g.peek_interrupt(), None);
    g.eoi(3).unwrap();
    assert_eq!(g.peek_interrupt(), Some(991));
    g.set_pmr(0);
    assert_eq!(g.peek_interrupt(), None);
    g.set_pmr(255);
    assert_eq!(g.take_interrupt(), Some(991));

    let mut tied = populated(960, 0);
    tied.priority[..992].fill(200);
    tied.priority[4] = 100;
    tied.priority[900] = 100;
    assert_eq!(tied.take_interrupt(), Some(4));
    assert_eq!(tied.peek_interrupt(), None);
    tied.eoi(4).unwrap();
    assert_eq!(tied.take_interrupt(), Some(900));
}
