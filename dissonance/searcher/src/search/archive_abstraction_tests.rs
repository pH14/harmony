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
    offer_in_job(archive, state, key, cost, u64::try_from(state).unwrap())
}

fn offer_in_job(
    archive: &mut ToyArchive,
    state: usize,
    key: Key,
    cost: usize,
    execution: u64,
) -> Option<usize> {
    archive
        .insert(
            None,
            execution,
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

#[test]
fn job_sample_preserves_both_extrema_of_every_fixed_stream_prefix() {
    let mut archive = ToyArchive::new(|_| 1);
    archive.slot_retention = SlotRetentionPolicy::RepresentativeJobSample2;
    let mut seen = Vec::new();
    for state in 0..64 {
        let key = Key {
            slot: 0,
            resources: [u64::try_from((state * 13) % 17).unwrap(), 0],
        };
        let cost = 1 + state % 7;
        offer(&mut archive, state, key, cost);
        seen.push((state, key.resources, cost));
        let best = seen
            .iter()
            .max_by_key(|(id, r, c)| (r[1], r[0], Reverse(c), Reverse(id)))
            .unwrap()
            .0;
        let sampled = seen
            .iter()
            .min_by_key(|(id, _, _)| retention_job_rank(u64::try_from(*id).unwrap()))
            .unwrap()
            .0;
        let actual: BTreeSet<_> = archive
            .entries
            .iter()
            .enumerate()
            .filter(|(id, _)| archive.active[*id])
            .map(|(_, entry)| *entry.snapshot.as_deref().unwrap())
            .collect();
        assert_eq!(actual, BTreeSet::from([best, sampled]));
        assert!(archive.active_count() <= 2);
    }
}

#[test]
fn job_sample_can_lose_a_useful_state_within_one_cohort() {
    let model = Model {
        labels: vec![0; 3],
        edges: vec![[edge(2, 1); 2], [edge(2, 0); 2], [edge(2, 0); 2]],
    };
    let mut archive = ToyArchive::new(|_| 1);
    archive.slot_retention = SlotRetentionPolicy::RepresentativeJobSample2;
    offer_in_job(
        &mut archive,
        0,
        Key {
            slot: 0,
            resources: [1, 1],
        },
        1,
        7,
    );
    assert!(retained_can_reach(&archive, &model, 1));
    offer_in_job(
        &mut archive,
        1,
        Key {
            slot: 0,
            resources: [2, 2],
        },
        2,
        7,
    );
    assert_eq!(archive.active_count(), 1);
    assert!(!retained_can_reach(&archive, &model, 1));
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct ContextKey {
    quality: u8,
    context: Option<u64>,
}

impl ArchiveKey for ContextKey {
    type Group = ();
    type Lineage = ();
    fn groups() -> usize {
        1
    }
    fn group(self, depth: usize) {
        assert_eq!(depth, 0);
    }
    fn slot_capacity() -> usize {
        1
    }
    fn preference_cmp(self, other: Self) -> Ordering {
        self.quality.cmp(&other.quality)
    }
    fn retention_context(self) -> Option<u64> {
        self.context
    }
    fn complete(self, _: Option<(Self, &())>) -> Self {
        self
    }
    fn record(_: &mut (), _: Self) {}
}

type ContextArchive = Archive<u8, ContextKey, (), usize>;

fn offer_context(archive: &mut ContextArchive, state: usize, quality: u8, context: Option<u64>) {
    archive
        .insert(
            None,
            u64::try_from(state).unwrap(),
            ArchiveCandidate {
                suffix: vec![u8::try_from(state).unwrap()],
                key: ContextKey { quality, context },
                milestones: (),
            },
            state,
        )
        .unwrap();
}

fn retained_context_states(archive: &ContextArchive) -> BTreeSet<usize> {
    archive
        .entries
        .iter()
        .enumerate()
        .filter(|(id, _)| archive.active[*id])
        .map(|(_, entry)| *entry.snapshot.as_deref().unwrap())
        .collect()
}

#[test]
fn context_and_quality_pairs_match_fixed_stream_top_two_oracles_at_every_prefix() {
    // Includes repeated contexts, a late new maximum, equal-quality arrivals,
    // and contexts that were excluded before their quality improved.
    let stream = [
        (4, 0),
        (3, 0),
        (2, 1),
        (1, 2),
        (5, 2),
        (6, 1),
        (6, 0),
        (7, 3),
        (8, 0),
        (9, 3),
        (8, 2),
        (10, 4),
    ];
    for policy in [
        SlotRetentionPolicy::QualityRepresentatives2,
        SlotRetentionPolicy::ContextRepresentatives2,
    ] {
        let mut archive = ContextArchive::new(|_| 1);
        archive.slot_retention = policy;
        for (index, &(quality, context)) in stream.iter().enumerate() {
            offer_context(&mut archive, index, quality, Some(context));
            let mut candidates: Vec<_> = stream[..=index].iter().enumerate().collect();
            candidates.sort_by_key(|(i, (q, _))| (Reverse(*q), *i));
            let mut contexts = BTreeSet::new();
            let expected: BTreeSet<_> = candidates
                .into_iter()
                .filter(|(_, (_, context))| {
                    policy == SlotRetentionPolicy::QualityRepresentatives2
                        || contexts.insert(*context)
                })
                .take(2)
                .map(|(i, _)| i)
                .collect();
            assert_eq!(
                retained_context_states(&archive),
                expected,
                "policy={policy:?} prefix={index}"
            );
        }
    }
}

#[test]
fn context_pair_preserves_a_distinct_exit_but_can_discard_a_third_context() {
    let model = Model {
        labels: vec![0; 5],
        edges: vec![
            [edge(0, 0); 2],
            [edge(1, 0); 2],
            [edge(4, 1), edge(2, 0)],
            [edge(3, 0), edge(4, 2)],
            [edge(4, 0); 2],
        ],
    };
    assert_eq!(model.distinguish(0, 2), Some(vec![0]));
    for policy in [
        SlotRetentionPolicy::QualityRepresentatives2,
        SlotRetentionPolicy::ContextRepresentatives2,
    ] {
        let mut archive = ContextArchive::new(|_| 1);
        archive.slot_retention = policy;
        // A superseded low-quality seed leaves genuine historical metadata.
        offer_context(&mut archive, 5, 0, Some(3));
        for (state, (quality, context)) in [(10, 0), (9, 0), (8, 1), (7, 2)].into_iter().enumerate()
        {
            offer_context(&mut archive, state, quality, Some(context));
        }
        let retained = retained_context_states(&archive);
        assert_eq!(retained.len(), 2);
        // Historical entries can remain for reconstruction. They must not
        // inflate the final census, whether or not their snapshots are cached.
        assert!(archive.entries.len() > retained.len());
        let census = archive.retention_context_census().unwrap();
        assert_eq!(census["active_entries"], 2);
        assert_eq!(census["largest_slot"], 2);
        assert_eq!(census["with_context"], 2);
        assert_eq!(
            census["two_distinct_contexts"],
            u64::from(policy == SlotRetentionPolicy::ContextRepresentatives2)
        );
        assert_eq!(
            retained.iter().any(|s| model.can_reach_event(*s, 1)),
            policy == SlotRetentionPolicy::ContextRepresentatives2
        );
        assert!(model.can_reach_event(3, 2));
        assert!(!retained.iter().any(|s| model.can_reach_event(*s, 2)));
    }
}

#[test]
fn absent_context_uses_ordinary_capacity_instead_of_inventing_diversity() {
    let mut archive = ContextArchive::new(|_| 1);
    archive.slot_retention = SlotRetentionPolicy::ContextRepresentatives2;
    for (state, quality) in [4, 5, 2, 6].into_iter().enumerate() {
        offer_context(&mut archive, state, quality, None);
    }
    assert_eq!(retained_context_states(&archive), BTreeSet::from([3]));
    assert_eq!(archive.retention_diagnostics.alternative_admissions, 0);
}

#[test]
fn equal_policy_averaged_event_features_can_hide_opposite_action_futures() {
    let model = Model {
        labels: vec![0, 0, 1, 2],
        edges: vec![
            [edge(2, 1), edge(3, 0)],
            [edge(3, 0), edge(2, 1)],
            [edge(2, 0); 2],
            [edge(3, 0); 2],
        ],
    };
    // The uniform policy gives the same one-half event probability at both
    // starts. Future steps emit nothing, so every longer event-return average
    // also agrees. This is insufficient for preserving action-conditioned use.
    let event_numerator = |state: usize| {
        model.edges[state]
            .iter()
            .map(|e| u32::from(e.events))
            .sum::<u32>()
    };
    assert_eq!(event_numerator(0), 1);
    assert_eq!(event_numerator(1), 1);
    assert_eq!(model.distinguish(0, 1), Some(vec![0]));
    let refined = model.refine(vec![0, 0, 1, 2]);
    assert_ne!(refined[0], refined[1]);
    // Changing the action mixture to P(action0)=3/4 reverses the values.
    let weighted = |state: usize| {
        3 * u32::from(model.edges[state][0].events) + u32::from(model.edges[state][1].events)
    };
    assert_eq!((weighted(0), weighted(1)), (3, 1));
}
