// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exhaustive finite counterexamples for archive abstraction claims.
//! The toy worlds are exact; only admission uses the production engine.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Edge {
    next: usize,
    cost: u8,
    events: u8,
}

struct Model {
    labels: Vec<u8>, // Includes terminal/task labels.
    edges: Vec<[Edge; 2]>,
}

impl Model {
    /// Shortest distinguishing suffix, or exact equivalence in this finite model.
    fn distinguish(&self, left: usize, right: usize) -> Option<Vec<u8>> {
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::from([(left, right, Vec::new())]);
        while let Some((left, right, prefix)) = queue.pop_front() {
            if !seen.insert((left, right)) {
                continue;
            }
            if self.labels[left] != self.labels[right] {
                return Some(prefix);
            }
            for action in 0..2 {
                let (l, r) = (self.edges[left][action], self.edges[right][action]);
                let mut suffix = prefix.clone();
                suffix.push(u8::try_from(action).unwrap());
                if (l.cost, l.events, self.labels[l.next])
                    != (r.cost, r.events, self.labels[r.next])
                {
                    return Some(suffix);
                }
                queue.push_back((l.next, r.next, suffix));
            }
        }
        None
    }

    /// Coarsest stable refinement of the supplied partition (no cells merge).
    fn refine(&self, mut cells: Vec<usize>) -> Vec<usize> {
        loop {
            let mut signatures = BTreeMap::new();
            let next: Vec<_> = self
                .edges
                .iter()
                .enumerate()
                .map(|(s, edges)| {
                    let signature = (
                        cells[s],
                        self.labels[s],
                        edges.map(|e| (e.cost, e.events, cells[e.next])),
                    );
                    let fresh = signatures.len();
                    *signatures.entry(signature).or_insert(fresh)
                })
                .collect();
            // Canonical IDs are assigned in state order. The first pass can
            // relabel the supplied partition; subsequent passes are stable.
            if next == cells {
                return cells;
            }
            cells = next;
        }
    }

    fn can_reach_event(&self, start: usize, event: u8) -> bool {
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::from([start]);
        while let Some(state) = queue.pop_front() {
            if !seen.insert(state) {
                continue;
            }
            for edge in self.edges[state] {
                if edge.events & event != 0 {
                    return true;
                }
                queue.push_back(edge.next);
            }
        }
        false
    }
}

fn edge(next: usize, events: u8) -> Edge {
    Edge {
        next,
        cost: 1,
        events,
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct Key {
    slot: usize,
    resources: [u64; 2],
}

impl ArchiveKey for Key {
    type Group = usize;
    type Lineage = ();
    fn groups() -> usize {
        1
    }
    fn group(self, depth: usize) -> usize {
        assert_eq!(depth, 0);
        self.slot
    }
    fn slot_capacity() -> usize {
        1
    }
    fn preference_cmp(self, other: Self) -> Ordering {
        (self.resources[1], self.resources[0]).cmp(&(other.resources[1], other.resources[0]))
    }
    fn retention_resources(self) -> Option<[u64; 2]> {
        Some(self.resources)
    }
    fn complete(self, _: Option<(Self, &())>) -> Self {
        self
    }
    fn record(_: &mut (), _: Self) {}
}

type ToyArchive = Archive<u8, Key, (), usize>;

fn offer(archive: &mut ToyArchive, state: usize, key: Key, cost: usize) -> Option<usize> {
    archive
        .insert(
            None,
            u64::try_from(state).unwrap(),
            ArchiveCandidate {
                // Unique ordinary inputs avoid the archive's exact-input dedup path.
                suffix: vec![u8::try_from(state).unwrap(); cost],
                key,
                milestones: (),
            },
            state,
        )
        .unwrap()
}

fn retained_can_reach(archive: &ToyArchive, model: &Model, event: u8) -> bool {
    archive.entries.iter().enumerate().any(|(id, entry)| {
        archive.active[id]
            && entry
                .snapshot
                .as_deref()
                .is_some_and(|s| model.can_reach_event(*s, event))
    })
}

#[test]
fn count_alias_loses_a_future_and_stable_refinement_preserves_it() {
    // Equal-count capabilities at one location: only state 0 opens the exit.
    // State 1 reaches the same abstract location through a shorter prefix.
    let model = Model {
        labels: vec![0, 0, 0],
        edges: vec![
            [edge(2, 1), edge(0, 0)],
            [edge(2, 0), edge(1, 0)],
            [edge(2, 0); 2],
        ],
    };
    assert_eq!(model.distinguish(0, 1), Some(vec![0]));
    let coarse = Key {
        slot: 1,
        resources: [5, 5],
    };
    let mut archive = ToyArchive::new(|_| 1);
    offer(&mut archive, 0, coarse, 2).unwrap();
    assert!(retained_can_reach(&archive, &model, 1));
    offer(&mut archive, 1, coarse, 1).unwrap();
    assert_eq!(archive.active_count(), 1);
    assert!(!retained_can_reach(&archive, &model, 1));

    let refined = model.refine(vec![0; 3]);
    assert_ne!(refined[0], refined[1]);
    let mut archive = ToyArchive::new(|_| 1);
    for (state, cost) in [(0, 2), (1, 1)] {
        offer(
            &mut archive,
            state,
            Key {
                slot: refined[state],
                ..coarse
            },
            cost,
        )
        .unwrap();
    }
    assert!(retained_can_reach(&archive, &model, 1));
    // Exhaustively check the final partition's equivalence assertion.
    for left in 0..3 {
        for right in 0..3 {
            if refined[left] == refined[right] {
                assert_eq!(model.distinguish(left, right), None);
            }
        }
    }
}

#[test]
fn resource_extremes_lose_a_monotone_threshold_exit() {
    let resources = [[10, 0], [5, 5], [0, 10]];
    let model = Model {
        labels: vec![0; 4],
        edges: resources
            .iter()
            .enumerate()
            .map(|(s, r)| [edge(3, u8::from(r[0] >= 3 && r[1] >= 3)), edge(s, 0)])
            .chain(std::iter::once([edge(3, 0); 2]))
            .collect(),
    };
    assert!(model.can_reach_event(1, 1));
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let mut archive = ToyArchive::new(|_| 1);
        archive.slot_retention = SlotRetentionPolicy::ResourceExtremes2;
        for state in order {
            offer(
                &mut archive,
                state,
                Key {
                    slot: 0,
                    resources: resources[state],
                },
                1,
            );
        }
        assert_eq!(archive.active_count(), 2);
        assert!(
            !retained_can_reach(&archive, &model, 1),
            "admission order {order:?}"
        );
    }
}

#[test]
fn threshold_coverage_preserves_the_middle_exit_at_the_same_capacity() {
    let resources = [[10, 0], [5, 5], [0, 10]];
    let model = Model {
        labels: vec![0; 4],
        edges: resources
            .iter()
            .enumerate()
            .map(|(s, r)| [edge(3, u8::from(r[0] >= 3 && r[1] >= 3)), edge(s, 0)])
            .chain(std::iter::once([edge(3, 0); 2]))
            .collect(),
    };
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let mut archive = ToyArchive::new(|_| 1);
        archive.slot_retention = SlotRetentionPolicy::ResourceCoverage2;
        for state in order {
            offer(
                &mut archive,
                state,
                Key {
                    slot: 0,
                    resources: resources[state],
                },
                1,
            );
        }
        assert_eq!(archive.active_count(), 2);
        assert!(
            retained_can_reach(&archive, &model, 1),
            "admission order {order:?}"
        );
    }
}

#[test]
fn equal_endpoints_do_not_hide_interior_events_or_actual_cost() {
    let mut model = Model {
        labels: vec![0; 3],
        edges: vec![[edge(2, 0); 2]; 3],
    };
    assert_eq!(model.distinguish(0, 1), None);
    model.edges[0][1].events = 1;
    assert_eq!(model.distinguish(0, 1), Some(vec![1]));
    model.edges[0][1].events = 0;
    model.edges[1][0].cost = 2;
    assert_eq!(model.distinguish(0, 1), Some(vec![0]));
}

#[test]
fn product_search_finds_late_distinctions_and_terminates_on_equivalent_cycles() {
    let model = Model {
        labels: vec![0; 5],
        edges: vec![
            [edge(2, 0), edge(0, 0)],
            [edge(3, 0), edge(1, 0)],
            [edge(4, 1), edge(2, 0)],
            [edge(4, 0), edge(3, 0)],
            [edge(4, 0); 2],
        ],
    };
    assert_eq!(model.distinguish(0, 1), Some(vec![0, 0]));
    assert_eq!(model.distinguish(1, 3), None);
    let refined = model.refine(vec![0; 5]);
    for left in 0..5 {
        for right in 0..5 {
            assert_eq!(
                refined[left] == refined[right],
                model.distinguish(left, right).is_none()
            );
        }
    }
}

#[test]
fn a_short_label_difference_precedes_a_longer_cost_difference() {
    let model = Model {
        labels: vec![0, 0, 0, 0, 0, 1],
        edges: vec![
            [edge(2, 0), edge(4, 0)],
            [edge(3, 0), edge(5, 0)],
            [edge(2, 0); 2],
            [Edge {
                next: 3,
                cost: 2,
                events: 0,
            }; 2],
            [edge(4, 0); 2],
            [edge(5, 0); 2],
        ],
    };
    assert_eq!(model.distinguish(0, 1), Some(vec![1]));
}

#[test]
fn retaining_an_extra_future_can_reduce_one_attempt_goal_discovery() {
    // The second state supplies a distinct possible resource threshold but
    // does not help goal 1. Both archives retain the state reaching goal 1.
    let model = Model {
        labels: vec![0; 3],
        edges: vec![
            [edge(2, 1), edge(0, 0)],
            [edge(2, 2), edge(1, 0)],
            [edge(2, 0); 2],
        ],
    };
    let mut one = ToyArchive::new(|_| 1);
    let mut two = ToyArchive::new(|_| 1);
    two.slot_retention = SlotRetentionPolicy::ResourceCoverage2;
    let good = Key {
        slot: 0,
        resources: [10, 1],
    };
    offer(&mut one, 0, good, 1).unwrap();
    offer(&mut two, 0, good, 1).unwrap();
    offer(
        &mut two,
        1,
        Key {
            slot: 0,
            resources: [1, 10],
        },
        1,
    )
    .unwrap();
    assert!(retained_can_reach(&two, &model, 1));
    assert!(retained_can_reach(&two, &model, 2));
    assert!(!retained_can_reach(&one, &model, 2));
    let mut successes = [0; 2];
    // Exhaust this declared finite seed panel with the production selector.
    // Each draw is the first attempt: no result counters are admitted.
    for seed in 0..1024 {
        for (arm, archive) in [&mut one, &mut two].into_iter().enumerate() {
            let (id, _) = archive
                .select_parent(&mut RomuDuoJrRand::with_seed(seed), 8)
                .unwrap();
            let state = *archive.entries[id].snapshot.as_deref().unwrap();
            successes[arm] += usize::from(model.edges[state][0].events & 1 != 0);
        }
    }
    assert_eq!(successes[0], 1024);
    assert!(
        successes[1] > 0 && successes[1] < successes[0],
        "{successes:?}"
    );
}

#[test]
fn local_coverage_optimum_can_forget_a_later_useful_complement() {
    // A,B cover 27 thresholds; B,C cover 26 and A,C cover 21. Therefore
    // coverage discards C without a tie. Later D dominates A and B; D,C
    // would cover 61 thresholds, but only D (55) remains available.
    let resources = [[0, 10], [4, 3], [10, 0], [4, 10]];
    let model = Model {
        labels: vec![0; 5],
        edges: resources
            .iter()
            .enumerate()
            .map(|(s, r)| [edge(4, u8::from(r[0] >= 10)), edge(s, 0)])
            .chain(std::iter::once([edge(4, 0); 2]))
            .collect(),
    };
    for (policy, expected_reach) in [
        (SlotRetentionPolicy::ResourceCoverage2, false),
        (SlotRetentionPolicy::ResourceExtremes2, true),
    ] {
        let mut archive = ToyArchive::new(|_| 1);
        archive.slot_retention = policy;
        for (state, resources) in resources.into_iter().enumerate() {
            offer(&mut archive, state, Key { slot: 0, resources }, 1);
        }
        assert_eq!(retained_can_reach(&archive, &model, 1), expected_reach);
    }
}
