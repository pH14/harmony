// SPDX-License-Identifier: AGPL-3.0-or-later
//! Finite allocation checks and counterexamples in the production selector.
use super::*;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct Key {
    slot: u16,
    resources: Option<[u64; 2]>,
    progress: Option<ScopedProgress>,
}
impl ArchiveKey for Key {
    type Group = u16;
    type Lineage = ();
    fn groups() -> usize {
        2
    }
    fn group(self, depth: usize) -> u16 {
        match depth {
            0 => self.slot,
            1 => 0,
            _ => panic!("invalid depth"),
        }
    }
    fn slot_capacity() -> usize {
        1
    }
    fn preference_cmp(self, other: Self) -> Ordering {
        self.resources.cmp(&other.resources)
    }
    fn retention_resources(self) -> Option<[u64; 2]> {
        self.resources
    }
    fn retention_progress(self) -> Option<ScopedProgress> {
        self.progress
    }
    fn complete(self, _: Option<(Self, &())>) -> Self {
        self
    }
    fn record(_: &mut (), _: Self) {}
}
type Toy = Archive<u16, Key, (), usize>;
fn key(slot: u16, value: u64) -> Key {
    Key {
        slot,
        resources: Some([79, 0]),
        progress: Some(ScopedProgress { scope: 7, value }),
    }
}
fn offer(a: &mut Toy, id: usize, key: Key, cost: usize) {
    a.insert(
        None,
        id as u64,
        ArchiveCandidate {
            suffix: vec![u16::try_from(id).unwrap(); cost],
            key,
            milestones: (),
        },
        id,
    )
    .unwrap();
}
fn policy(mode: u8) -> SelectorPolicy {
    let t = RetireThresholds {
        entry: 3,
        groups: vec![],
    };
    match mode {
        0 => SelectorPolicy::EnergyProgressCheapest(t),
        1 => SelectorPolicy::EnergyProgressCheapestScopedReturnControl(t),
        2 => SelectorPolicy::EnergyProgressCheapestScopedReturnHalf(t),
        _ => panic!("invalid mode"),
    }
}
fn pair() -> Toy {
    let mut a = Toy::new(|_| 1);
    a.slot_retention = SlotRetentionPolicy::ResourceGuardedProgress2;
    offer(&mut a, 0, key(0, 114), 1);
    offer(&mut a, 1, key(0, 117), 8);
    a
}

#[test]
fn identifiers_round_trip_and_reject_unversioned_or_invalid_shapes() {
    for (mode, name) in [(1, "control"), (2, "half")] {
        let id =
            format!("room_cell_uniform_128_energy_progress_cheapest_scoped_return_{name}_v1:3");
        assert_eq!(selector_policy_identifier(&policy(mode)), id);
        assert_eq!(
            selector_policy_from_identifier(&id, 0).unwrap(),
            policy(mode)
        );
        assert!(selector_policy_from_identifier(&id, 1).is_err());
        assert!(selector_policy_from_identifier(&id.replace(":3", ":0"), 0).is_err());
        assert!(selector_policy_from_identifier(&id.replace("_v1", ""), 0).is_err());
    }
}

#[test]
fn production_draw_couples_the_proposal_coin_and_uniform_path() {
    let mut a = pair();
    // Four fillers put progress at cost rank 5, beyond the first halving.
    for id in 2..6 {
        offer(&mut a, id, key(u16::try_from(id).unwrap(), 114), 2);
    }
    let mut redirected = 0;
    let mut uniform = 0;
    let mut proposed = [0_usize; 2];
    for seed in 0..512 {
        a.in_window_ever.fill(false);
        a.selector_policy = policy(0);
        a.frontier_cap = None;
        let mut base_rng = RomuDuoJrRand::with_seed(seed);
        let (base, draw) = a.select_parent(&mut base_rng, 100).unwrap();
        let walk = matches!(draw.path, SelectorPath::GroupWalk);
        let coin = walk && base_rng.below(NonZeroUsize::new(2).unwrap()) != 0;
        let next = base_rng.next_u64();
        for mode in [1, 2] {
            a.in_window_ever.fill(false);
            a.selector_policy = policy(mode);
            a.frontier_cap = None;
            let mut rng = RomuDuoJrRand::with_seed(seed);
            let (selected, actual) = a.select_parent(&mut rng, 100).unwrap();
            assert_eq!(actual, draw);
            assert_eq!(
                rng.next_u64(),
                next,
                "exactly one coin on walk, none on uniform"
            );
            let expected = if mode == 2 && coin && base == 0 {
                1
            } else {
                base
            };
            assert_eq!(selected, expected);
            if mode == 2 && selected != base {
                redirected += 1;
            }
        }
        if walk && base < 2 {
            proposed[base] += 1;
        }
        if !walk {
            uniform += 1;
        }
    }
    assert!(redirected > 0 && uniform > 0);
    assert!(
        proposed[0] > proposed[1] && proposed[1] > 0,
        "planted cost disadvantage is exercised"
    );
    // Enumerate both fair-coin outcomes for this finite production proposal
    // population. Exact allocation identity; no sampled campaign-level bound.
    let window: Vec<_> = (0..6).collect();
    let high: usize = proposed
        .iter()
        .enumerate()
        .map(|(id, count)| {
            count * (usize::from(id == 1) + usize::from(a.scoped_return_parent(id, &window) == 1))
        })
        .sum();
    assert_eq!(high, 2 * proposed[1] + proposed[0]);
    let total = 2 * (proposed[0] + proposed[1]);
    // Adverse world: only the ordinary member's next continuation succeeds.
    // Its mass strictly decreases but remains positive. Progress is no oracle.
    let ordinary_successes = total - high;
    assert_eq!(ordinary_successes, proposed[0]);
    assert!(ordinary_successes > 0 && ordinary_successes < 2 * proposed[0]);
}

#[test]
fn no_redirect_without_strict_same_scope_resource_qualification() {
    let base = key(0, 114);
    let mut cases = vec![base, key(0, 110)];
    let mut unknown_progress = key(0, 117);
    unknown_progress.progress = None;
    let mut unknown_resources = key(0, 117);
    unknown_resources.resources = None;
    let mut other_scope = key(0, 117);
    other_scope.progress.as_mut().unwrap().scope += 1;
    let mut resource_loss = key(0, 117);
    resource_loss.resources = Some([78, 1]);
    cases.extend([
        unknown_progress,
        unknown_resources,
        other_scope,
        resource_loss,
    ]);
    for invalid in cases {
        let mut a = pair();
        // Also exercise already-retained pairs whose keys no longer qualify;
        // the selector must check qualification, not infer it from slot size.
        a.entries[1].key = invalid;
        assert_eq!(a.scoped_return_parent(0, &[0, 1]), 0);
    }
    let mut a = pair();
    a.entries[0].key.progress = None;
    assert_eq!(a.scoped_return_parent(0, &[0, 1]), 0);
    a.entries[0].key = base;
    a.entries[0].key.resources = None;
    assert_eq!(a.scoped_return_parent(0, &[0, 1]), 0);
    let mut a = pair();
    for retention in [
        SlotRetentionPolicy::ResourceGuardedProgress2,
        SlotRetentionPolicy::ResourceGuardedProgressQuality2,
    ] {
        a.slot_retention = retention;
        assert_eq!(a.scoped_return_parent(0, &[0, 1]), 1);
        assert_eq!(a.scoped_return_parent(1, &[0, 1]), 1);
    }
    for retention in [
        SlotRetentionPolicy::Representative,
        SlotRetentionPolicy::ResourceExtremes2,
    ] {
        a.slot_retention = retention;
        assert_eq!(a.scoped_return_parent(0, &[0, 1]), 0);
    }
}

#[test]
fn production_walk_preserves_eligibility_and_the_existing_reset() {
    for exclusion in ["inactive", "origin", "actions", "exhausted"] {
        let mut a = pair();
        a.selector_policy = policy(2);
        match exclusion {
            "inactive" => {
                assert!(a.deactivate(1));
            }
            "origin" => {
                a.snapshot_selectable[1] = false;
                assert!(!a.origin_resident(1));
            }
            "exhausted" => a.since_retained[1] = 3,
            "actions" => (),
            _ => unreachable!(),
        }
        let cap = if exclusion == "actions" { 8 } else { 100 };
        let mut walks = 0;
        for seed in 0..64 {
            let (id, draw) = a
                .select_parent(&mut RomuDuoJrRand::with_seed(seed), cap)
                .unwrap();
            if matches!(draw.path, SelectorPath::GroupWalk) {
                walks += 1;
                assert_eq!(id, 0, "{exclusion}");
                assert!(!draw.counter_reset);
            } else if exclusion != "exhausted" {
                assert_eq!(id, 0, "uniform still excludes {exclusion}");
            }
        }
        assert!(walks > 0);
    }
    let mut a = pair();
    a.selector_policy = policy(2);
    a.since_retained.fill(3);
    for seed in 0..64 {
        let (id, draw) = a
            .select_parent(&mut RomuDuoJrRand::with_seed(seed), 100)
            .unwrap();
        if matches!(draw.path, SelectorPath::GroupWalk) {
            assert!(draw.counter_reset);
            assert_eq!(
                a.since_retained,
                [3, 3],
                "reset is deferred until stream application"
            );
            a.record_selection(id, &draw);
            assert_eq!(a.since_retained[id], 1);
            assert_eq!(a.since_retained[1 - id], 0);
            return;
        }
    }
    panic!("must exercise reset walk");
}

#[test]
fn a_progress_alternate_outside_the_recency_window_cannot_return() {
    let mut a = Toy::new(|_| 1);
    a.slot_retention = SlotRetentionPolicy::ResourceGuardedProgress2;
    a.selector_policy = policy(2);
    offer(&mut a, 0, key(0, 117), 8);
    offer(&mut a, 1, key(0, 114), 1);
    for id in 2..=CONCENTRATION_WINDOW {
        offer(&mut a, id, key(u16::try_from(id).unwrap(), 114), 2);
    }
    let mut ordinary = 0;
    for seed in 0..512 {
        let (id, draw) = a
            .select_parent(&mut RomuDuoJrRand::with_seed(seed), 100)
            .unwrap();
        if matches!(draw.path, SelectorPath::GroupWalk) {
            assert_ne!(id, 0);
            assert_eq!(draw.concentration.unwrap().window_size, 128);
            ordinary += usize::from(id == 1);
        }
    }
    assert!(ordinary > 0 && !a.in_window_ever[0]);
}
