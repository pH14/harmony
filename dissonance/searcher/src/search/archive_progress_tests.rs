// SPDX-License-Identifier: AGPL-3.0-or-later
//! Production-archive counterexamples for scoped progress retention.
use super::*;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct Key {
    resources: Option<[u64; 2]>,
    progress: Option<ScopedProgress>,
}
impl ArchiveKey for Key {
    type Group = u8;
    type Lineage = ();
    fn groups() -> usize {
        1
    }
    fn group(self, depth: usize) -> u8 {
        assert_eq!(depth, 0);
        0
    }
    fn slot_capacity() -> usize {
        1
    }
    fn preference_cmp(self, other: Self) -> Ordering {
        self.resources
            .map(|r| (r[1], r[0]))
            .cmp(&other.resources.map(|r| (r[1], r[0])))
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
type Toy = Archive<u8, Key, (), usize>;
fn key(value: u64) -> Key {
    Key {
        resources: Some([79, 0]),
        progress: Some(ScopedProgress {
            scope: 0x14000940,
            value,
        }),
    }
}
fn offer(a: &mut Toy, id: usize, key: Key, cost: usize) {
    a.insert(
        None,
        id as u64,
        ArchiveCandidate {
            suffix: vec![id as u8; cost],
            key,
            milestones: (),
        },
        id,
    )
    .unwrap();
}
fn active(a: &Toy) -> BTreeSet<usize> {
    a.entries
        .iter()
        .enumerate()
        .filter(|(i, _)| a.active[*i])
        .map(|(_, e)| *e.snapshot.as_deref().unwrap())
        .collect()
}
fn policies() -> [SlotRetentionPolicy; 2] {
    [
        SlotRetentionPolicy::ResourceGuardedProgress2,
        SlotRetentionPolicy::ResourceGuardedProgressQuality2,
    ]
}

#[test]
fn preserves_the_observed_distinction_and_separates_progress_from_capacity() {
    // PC01's equal-resource HP140/137 distinction maps to values114/117.
    // Costs are planted ordinary-preference ties, not reconstructed pilot costs.
    for (policy, expected) in [
        (SlotRetentionPolicy::Representative, BTreeSet::from([0])),
        (policies()[0], BTreeSet::from([0, 2])),
        (policies()[1], BTreeSet::from([0, 1])),
    ] {
        let mut a = Toy::new(|_| 1);
        a.slot_retention = policy;
        for (id, value) in [114, 117, 123].into_iter().enumerate() {
            offer(&mut a, id, key(value), id + 1);
            assert!(a.active_count() <= 2);
        }
        assert_eq!(active(&a), expected);
    }
}

#[test]
fn missing_cross_scope_equal_progress_and_resource_tradeoffs_are_ineligible() {
    for policy in policies() {
        let mut a = Toy::new(|_| 1);
        a.slot_retention = policy;
        offer(&mut a, 0, key(114), 1);
        let mut different_scope = key(200);
        different_scope.progress.as_mut().unwrap().scope += 1;
        let mut missing = key(200);
        missing.progress = None;
        let mut lower_resource = key(200);
        lower_resource.resources = Some([78, 0]);
        let mut absent_resource = key(200);
        absent_resource.resources = None;
        for (i, k) in [
            key(114),
            key(110),
            different_scope,
            missing,
            lower_resource,
            absent_resource,
        ]
        .into_iter()
        .enumerate()
        {
            offer(&mut a, i + 1, k, i + 2);
            assert_eq!(active(&a), BTreeSet::from([0]));
        }
        offer(&mut a, 7, key(117), 8);
        assert_eq!(active(&a), BTreeSet::from([0, 7]));
        // Stronger ordinary resources with unavailable progress must still win.
        offer(
            &mut a,
            8,
            Key {
                resources: Some([80, 0]),
                progress: None,
            },
            9,
        );
        assert_eq!(active(&a), BTreeSet::from([8]));
        let mut opposed = Toy::new(|_| 1);
        opposed.slot_retention = policy;
        offer(
            &mut opposed,
            0,
            Key {
                resources: Some([79, 1]),
                ..key(114)
            },
            1,
        );
        offer(
            &mut opposed,
            1,
            Key {
                resources: Some([100, 0]),
                ..key(200)
            },
            2,
        );
        assert_eq!(
            active(&opposed),
            BTreeSet::from([0]),
            "extra health cannot pay for lost missile stock in an alternate"
        );
    }
}

#[test]
fn offered_set_oracle_matches_every_prefix_of_permuted_streams() {
    let states = [
        key(114),
        key(117),
        key(123),
        Key {
            resources: Some([78, 0]),
            ..key(200)
        },
    ];
    for policy in policies() {
        for a in 0..4 {
            for b in 0..4 {
                for c in 0..4 {
                    for d in 0..4 {
                        let order = [a, b, c, d];
                        if order.into_iter().collect::<BTreeSet<_>>().len() != 4 {
                            continue;
                        }
                        let mut archive = Toy::new(|_| 1);
                        archive.slot_retention = policy;
                        for id in order {
                            let mut offered = active(&archive);
                            offered.insert(id);
                            let quality =
                                |i: &usize| (states[*i].resources.unwrap()[0], Reverse(*i));
                            let anchor = *offered.iter().max_by_key(|i| quality(i)).unwrap();
                            let eligible = offered.iter().copied().filter(|i| {
                                states[*i].resources.unwrap()[0]
                                    >= states[anchor].resources.unwrap()[0]
                                    && states[*i].progress.unwrap().value
                                        > states[anchor].progress.unwrap().value
                            });
                            let other = if policy == policies()[0] {
                                eligible.max_by_key(|i| {
                                    (states[*i].progress.unwrap().value, quality(i))
                                })
                            } else {
                                eligible.max_by_key(|i| quality(i))
                            };
                            let expected = BTreeSet::from([anchor, other.unwrap_or(anchor)]);
                            offer(&mut archive, id, states[id], id + 1);
                            assert_eq!(
                                active(&archive),
                                expected,
                                "{policy:?}, order={order:?}, id={id}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn selected_progress_is_not_a_certificate_of_future_success() {
    let mut a = Toy::new(|_| 1);
    a.slot_retention = policies()[0];
    for (id, value) in [114, 117, 123].into_iter().enumerate() {
        offer(&mut a, id, key(value), id + 1);
    }
    // A finite world may put its only future success behind discarded state1.
    let can_succeed = [false, true, false];
    assert!(!active(&a).iter().any(|i| can_succeed[*i]));
    assert!(can_succeed.iter().any(|v| *v));
}

#[test]
fn both_policy_identities_round_trip_and_omission_is_ordinary() {
    for p in policies() {
        assert_eq!(
            SlotRetentionPolicy::from_identifier(p.identifier()).unwrap(),
            p
        );
    }
    assert_eq!(
        SlotRetentionPolicy::from_identifier(None).unwrap(),
        SlotRetentionPolicy::Representative
    );
    assert!(SlotRetentionPolicy::from_identifier(Some("resource_guarded_progress_2_v0")).is_err());
}

#[test]
fn workloads_with_two_ordinary_members_keep_their_ordinary_rule() {
    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct Multi(Key);
    impl ArchiveKey for Multi {
        type Group = u8;
        type Lineage = ();
        fn groups() -> usize {
            1
        }
        fn group(self, depth: usize) -> u8 {
            self.0.group(depth)
        }
        fn slot_capacity() -> usize {
            2
        }
        fn preference_cmp(self, other: Self) -> Ordering {
            self.0.preference_cmp(other.0)
        }
        fn retention_resources(self) -> Option<[u64; 2]> {
            self.0.retention_resources()
        }
        fn retention_progress(self) -> Option<ScopedProgress> {
            self.0.retention_progress()
        }
        fn complete(self, _: Option<(Self, &())>) -> Self {
            self
        }
        fn record(_: &mut (), _: Self) {}
    }
    for policy in policies() {
        let mut archive = Archive::<u8, Multi, (), usize>::new(|_| 1);
        archive.slot_retention = policy;
        for (id, value) in [114, 117, 123].into_iter().enumerate() {
            archive
                .insert(
                    None,
                    id as u64,
                    ArchiveCandidate {
                        suffix: vec![id as u8; id + 1],
                        key: Multi(key(value)),
                        milestones: (),
                    },
                    id,
                )
                .unwrap();
        }
        let kept: BTreeSet<_> = archive
            .entries
            .iter()
            .enumerate()
            .filter(|(i, _)| archive.active[*i])
            .map(|(_, e)| *e.snapshot.as_deref().unwrap())
            .collect();
        assert_eq!(kept, BTreeSet::from([0, 1]));
    }
}

#[test]
fn productive_word_is_learned_only_from_a_retained_qualified_same_slot_transition() {
    let mut a = Toy::new(|_| 1);
    a.slot_retention = SlotRetentionPolicy::ResourceGuardedProgress2;
    a.enable_continuations(Some(ContinuationLearning::ScopedProgress));
    offer(&mut a, 0, key(114), 1);
    assert!(
        a.pop_continuation().is_none(),
        "bootstrap supplies no learned word"
    );
    a.insert(
        Some(0),
        1,
        ArchiveCandidate {
            suffix: vec![0, 1],
            key: key(117),
            milestones: (),
        },
        1,
    )
    .unwrap();
    let trial = a
        .pop_continuation()
        .expect("retained improvement must enqueue a trial");
    assert_eq!((trial.parent, trial.donor, trial.leaf), (1, 0, 1));
    assert_eq!(trial.actions, [0, 1]);
    assert!(
        a.pop_continuation().is_none(),
        "one event supplies only one attempt"
    );
    assert!(a.progress_trial_parent_eligible(1, 100));
    assert!(
        !a.progress_trial_parent_eligible(1, 3),
        "action ceiling remains strict"
    );
    a.snapshot_selectable[1] = false;
    a.snapshot_selectable[0] = false;
    assert!(
        !a.progress_trial_parent_eligible(1, 100),
        "missing reconstruction origin is ineligible"
    );
    a.snapshot_selectable[1] = true;
    a.snapshot_selectable[0] = true;
    a.deactivate(1);
    assert!(
        !a.progress_trial_parent_eligible(1, 100),
        "stale ids cannot pin/revive a state"
    );

    for mut k in [key(114), key(110), key(117)] {
        if k.progress.as_ref().unwrap().value == 117 {
            k.resources = Some([78, 1]);
        }
        let mut a = Toy::new(|_| 1);
        a.slot_retention = SlotRetentionPolicy::ResourceGuardedProgress2;
        a.enable_continuations(Some(ContinuationLearning::ScopedProgress));
        offer(&mut a, 0, key(114), 1);
        a.insert(
            Some(0),
            1,
            ArchiveCandidate {
                suffix: vec![0, 1],
                key: k,
                milestones: (),
            },
            1,
        )
        .unwrap();
        assert!(a.pop_continuation().is_none());
    }
}

#[test]
fn retaining_an_ordinary_improvement_does_not_bypass_word_qualification() {
    let base = key(114);
    let better = Key {
        resources: Some([80, 0]),
        ..key(117)
    };
    let mut wrong_scope = better;
    wrong_scope.progress.as_mut().unwrap().scope += 1;
    for (from, to) in [
        (
            base,
            Key {
                progress: None,
                ..better
            },
        ),
        (
            Key {
                progress: None,
                ..base
            },
            better,
        ),
        (base, wrong_scope),
        (
            Key {
                resources: None,
                ..base
            },
            better,
        ),
        (
            base,
            Key {
                progress: base.progress,
                ..better
            },
        ),
        (
            base,
            Key {
                resources: Some([78, 1]),
                ..better
            },
        ),
    ] {
        let mut a = Toy::new(|_| 1);
        a.enable_continuations(Some(ContinuationLearning::ScopedProgress));
        offer(&mut a, 0, from, 1);
        let kept = a
            .insert(
                Some(0),
                1,
                ArchiveCandidate {
                    suffix: vec![1],
                    key: to,
                    milestones: (),
                },
                1,
            )
            .unwrap();
        assert!(kept.is_some(), "ordinary preference must retain {to:?}");
        assert!(!resource_guarded_progress(from, to));
        assert!(
            a.pop_continuation().is_none(),
            "from={from:?}, to={to:?}, kept={kept:?}"
        );
    }
    let mut a = Toy::new(|_| 1);
    a.enable_continuations(Some(ContinuationLearning::ScopedProgress));
    offer(&mut a, 0, base, 1);
    // A qualified transition rejected by ordinary retention supplies no word.
    assert!(resource_guarded_progress(base, key(117)));
    assert!(
        a.insert(
            Some(0),
            1,
            ArchiveCandidate {
                suffix: vec![1],
                key: key(117),
                milestones: (),
            },
            1
        )
        .unwrap()
        .is_none()
    );
    assert!(a.pop_continuation().is_none());
}
