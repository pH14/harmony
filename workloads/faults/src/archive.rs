// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, num::NonZeroUsize};

use serde::{Deserialize, Serialize};

use searcher::search::{
    archive::{
        ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting, entries_by_suffix,
    },
    rand::RomuDuoJrRand,
};

use crate::bundle::FaultVocabulary;
use crate::target::{FaultAction, FaultObservations};

pub use searcher::search::archive::MAX_ARCHIVE_ENTRIES;

pub const KEY_POLICY_IDENTIFIER: &str = "faultlab_lifecycle_events_v4";
pub const HOOKS_FINISHED_KEY_CAP: u64 = 8;
pub const REPLACEMENT_IDENTIFIER: &str = "fewest_guest_ticks";
pub const DURATION_IDENTIFIER: &str = "adaptive_wait_ticks_v2";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FaultArchiveGroup {
    sometimes: u64,
    hooks_finished: u64,
    hooks_running: u64,
    alive: u64,
    parked: u64,
    event_ready: u64,
    event_kill_fires: u64,
    event_park_fires: u64,
    checks_finished: u64,
    workload_running: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FaultArchiveKey {
    pub sometimes: u64,
    pub hooks_finished: u64,
    pub hooks_running: u64,
    pub alive: u64,
    pub parked: u64,
    pub event_ready: u64,
    pub event_kill_fires: u64,
    pub event_park_fires: u64,
    pub checks_finished: u64,
    pub workload_running: bool,
}

impl ArchiveKey for FaultArchiveKey {
    type Group = FaultArchiveGroup;

    fn groups() -> usize {
        3
    }

    fn group(self, depth: usize) -> Self::Group {
        let full = FaultArchiveGroup {
            sometimes: self.sometimes,
            hooks_finished: self.hooks_finished,
            hooks_running: self.hooks_running,
            alive: self.alive,
            parked: self.parked,
            event_ready: self.event_ready,
            event_kill_fires: self.event_kill_fires,
            event_park_fires: self.event_park_fires,
            checks_finished: self.checks_finished,
            workload_running: self.workload_running,
        };
        match depth {
            0 => full,
            1 => FaultArchiveGroup { alive: 0, ..full },
            _ => FaultArchiveGroup {
                sometimes: self.sometimes,
                ..FaultArchiveGroup::default()
            },
        }
    }

    fn slot_capacity() -> usize {
        1
    }

    type Lineage = ();

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

#[must_use]
pub fn archive_key(observations: &FaultObservations) -> FaultArchiveKey {
    FaultArchiveKey {
        sometimes: observations.sometimes_bitmap(),
        hooks_finished: observations.hooks_finished.min(HOOKS_FINISHED_KEY_CAP),
        hooks_running: observations
            .hooks_started
            .saturating_sub(observations.hooks_finished)
            .min(HOOKS_FINISHED_KEY_CAP),
        alive: observations.alive,
        parked: observations.parked.min(HOOKS_FINISHED_KEY_CAP),
        event_ready: observations.event_ready,
        event_kill_fires: observations.event_kill_fires.min(HOOKS_FINISHED_KEY_CAP),
        event_park_fires: observations.event_park_fires.min(HOOKS_FINISHED_KEY_CAP),
        checks_finished: observations.checks_finished.min(HOOKS_FINISHED_KEY_CAP),
        workload_running: observations.workload_started > observations.workload_finished,
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultMilestones {
    pub sometimes: u64,
    pub hooks_finished: u64,
    pub bug: bool,
}

#[must_use]
pub fn milestones(observations: &FaultObservations) -> FaultMilestones {
    FaultMilestones {
        sometimes: observations.sometimes_bitmap(),
        hooks_finished: observations.hooks_finished,
        bug: observations.is_bug(),
    }
}

pub fn merge_milestones(into: &mut FaultMilestones, from: FaultMilestones) {
    into.sometimes |= from.sometimes;
    into.hooks_finished = into.hooks_finished.max(from.hooks_finished);
    into.bug |= from.bug;
}

#[must_use]
pub fn milestone_key(value: FaultMilestones) -> (u32, u64) {
    (value.sometimes.count_ones(), value.hooks_finished)
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FaultProgressWatermark {
    pub sometimes_sites: u32,
    pub hooks_finished: u64,
    pub ticks: u64,
}

pub fn merge_progress_watermark(
    watermark: &mut FaultProgressWatermark,
    observations: &[FaultObservations],
) {
    for observation in observations {
        *watermark = (*watermark).max(FaultProgressWatermark {
            sometimes_sites: observation.sometimes_bitmap().count_ones(),
            hooks_finished: observation.hooks_finished,
            ticks: observation.ticks,
        });
    }
}

#[must_use]
pub fn action_cost(action: &FaultAction) -> u64 {
    crate::target::action_ticks(action)
}

pub const PAUSE_TICKS: [u32; 4] = [1, 5, 25, 100];
pub const VECTORS: [u32; 2] = [0x20, 0x30];
pub const PARK_HITS: [u32; 8] = [1, 2, 4, 8, 16, 32, 64, 128];
pub const PARK_HOLD_US: [u32; 3] = [500, 2_000, 10_000];

pub fn sample_action(
    rand: &mut RomuDuoJrRand,
    vocabulary: &FaultVocabulary,
    event_ready: u64,
) -> Result<FaultAction, Box<dyn Error>> {
    let pick = |rand: &mut RomuDuoJrRand, len: usize| -> Result<usize, Box<dyn Error>> {
        Ok(rand.below(NonZeroUsize::new(len).ok_or("empty fault vocabulary alternative")?))
    };
    let node = u16::try_from(pick(rand, usize::from(vocabulary.nodes()))?)?;
    let mut alternatives = vec![0, 1, 2, 3];
    if !vocabulary.hooks().is_empty() {
        alternatives.push(4);
    }
    if vocabulary.interrupt_injection() {
        alternatives.push(5);
    }
    if !vocabulary.places().is_empty() {
        alternatives.push(6);
    }
    let node_mask = u64::MAX >> (64 - vocabulary.nodes());
    let event_ready = event_ready & node_mask;
    if vocabulary.instrumented_events() && event_ready != 0 {
        alternatives.extend([7, 8]);
    }
    let event_node = |rand: &mut RomuDuoJrRand| -> Result<u16, Box<dyn Error>> {
        let index = pick(rand, event_ready.count_ones() as usize)?;
        let mut bits = event_ready;
        for _ in 0..index {
            bits &= bits - 1;
        }
        Ok(u16::try_from(bits.trailing_zeros())?)
    };
    match alternatives[pick(rand, alternatives.len())?] {
        0 => Ok(FaultAction::Wait(std::num::NonZeroU16::MIN)),
        1 => Ok(FaultAction::Kill(node)),
        2 => Ok(FaultAction::Pause(
            node,
            PAUSE_TICKS[pick(rand, PAUSE_TICKS.len())?],
        )),
        3 => Ok(FaultAction::Restart(node)),
        4 => Ok(FaultAction::Hook(
            vocabulary.hooks()[pick(rand, vocabulary.hooks().len())?],
        )),
        5 => Ok(FaultAction::Interrupt(VECTORS[pick(rand, VECTORS.len())?])),
        6 => Ok(FaultAction::Park {
            node,
            addr: vocabulary.places()[pick(rand, vocabulary.places().len())?],
            hits: PARK_HITS[pick(rand, PARK_HITS.len())?],
            hold_us: PARK_HOLD_US[pick(rand, PARK_HOLD_US.len())?],
        }),
        7 => Ok(FaultAction::EventKill {
            node: event_node(rand)?,
            rarity: u8::try_from(pick(rand, 64)?)?,
        }),
        _ => Ok(FaultAction::EventPark {
            node: event_node(rand)?,
            rarity: u8::try_from(pick(rand, 64)?)?,
            hold_us: PARK_HOLD_US[pick(rand, PARK_HOLD_US.len())?],
        }),
    }
}

pub type FaultProgressPoint = ProgressPoint<FaultMilestones, FaultProgressWatermark>;
pub type FaultArchiveEntryReport =
    ArchiveEntryReport<FaultAction, FaultArchiveKey, FaultMilestones>;
pub type FaultInput = searcher::search::archive::Input<FaultAction>;

pub const MAX_RECORDED_BUGS: usize = 64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultBugRecord {
    pub execution: u64,
    pub input: FaultInput,
    pub observations: FaultObservations,
}

#[must_use]
pub fn bug_outcome(bugs: &[FaultBugRecord]) -> (u64, Option<u64>) {
    (
        bugs.len() as u64,
        bugs.iter().map(|bug| bug.execution).min(),
    )
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultArchiveReport {
    pub seed: u64,
    pub root_seal: u64,
    pub horizon_nanos: u64,
    pub executions: u64,
    pub milestones: FaultMilestones,
    pub progress_watermark: FaultProgressWatermark,
    pub champion_input: FaultInput,
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<FaultArchiveEntryReport>,
    pub progress_curve: Vec<FaultProgressPoint>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    #[serde(default)]
    pub watchdog_cutoffs: u64,
    pub bugs: Vec<FaultBugRecord>,
    #[serde(default)]
    pub selector: SelectorAccounting,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::target::FaultStop;

    fn endpoint(sometimes: &[u32], hooks_finished: u64, alive: u64) -> FaultObservations {
        FaultObservations {
            sometimes: sometimes.iter().copied().collect::<BTreeSet<_>>(),
            hooks_finished,
            alive,
            ..FaultObservations::default()
        }
    }

    #[test]
    fn the_key_is_the_sites_the_progress_and_the_survivors() {
        let key = archive_key(&endpoint(&[0, 5], 3, 0b11));
        assert_eq!(
            key,
            FaultArchiveKey {
                sometimes: 0b10_0001,
                hooks_finished: 3,
                hooks_running: 0,
                alive: 0b11,
                parked: 0,
                ..FaultArchiveKey::default()
            }
        );
    }

    #[test]
    fn raw_event_sites_remain_diagnostic_only() {
        let first = FaultObservations {
            event_kill_site: 41,
            event_kill_fires: 1,
            ..FaultObservations::default()
        };
        let second = FaultObservations {
            event_kill_site: 999,
            ..first.clone()
        };
        assert_eq!(archive_key(&first), archive_key(&second));
        assert_ne!(first.event_kill_site, second.event_kill_site);
    }

    #[test]
    fn hooks_still_running_open_their_own_cell() {
        let overlapping = FaultObservations {
            hooks_started: 2,
            hooks_finished: 1,
            alive: 0b1,
            ..FaultObservations::default()
        };
        let alone = FaultObservations {
            hooks_started: 1,
            hooks_finished: 1,
            alive: 0b1,
            ..FaultObservations::default()
        };
        assert_ne!(archive_key(&overlapping), archive_key(&alone));
        assert_eq!(archive_key(&overlapping).hooks_running, 1);
        let many = FaultObservations {
            hooks_started: 100,
            ..FaultObservations::default()
        };
        assert_eq!(archive_key(&many).hooks_running, HOOKS_FINISHED_KEY_CAP);
    }

    #[test]
    fn repeated_hooks_stop_opening_cells_past_the_cap() {
        let at_cap = archive_key(&endpoint(&[1], HOOKS_FINISHED_KEY_CAP, 0b1));
        let past = archive_key(&endpoint(&[1], HOOKS_FINISHED_KEY_CAP + 1, 0b1));
        let far_past = archive_key(&endpoint(&[1], 100, 0b1));
        assert_eq!(at_cap, past);
        assert_eq!(at_cap, far_past);
        assert_ne!(
            archive_key(&endpoint(&[1], HOOKS_FINISHED_KEY_CAP - 1, 0b1)),
            at_cap
        );
        assert_eq!(
            milestones(&endpoint(&[1], 100, 0b1)).hooks_finished,
            100,
            "the milestone keeps the true count"
        );
    }

    #[test]
    fn liveness_separates_slots_but_pools_one_depth_up() {
        let survived = archive_key(&endpoint(&[1], 2, 0b11));
        let lost_a_node = archive_key(&endpoint(&[1], 2, 0b01));
        assert_ne!(survived.group(0), lost_a_node.group(0));
        assert_eq!(survived.group(1), lost_a_node.group(1));
        assert_eq!(survived.group(2), lost_a_node.group(2));
        assert_eq!(FaultArchiveKey::slot_capacity(), 1);
    }

    #[test]
    fn hook_progress_separates_the_middle_depth() {
        let early = archive_key(&endpoint(&[1], 1, 0b1));
        let late = archive_key(&endpoint(&[1], 4, 0b1));
        assert_ne!(early.group(1), late.group(1));
        assert_eq!(early.group(2), late.group(2));
    }

    #[test]
    fn milestones_union_the_sites_and_latch_the_bug() {
        let mut aggregate = milestones(&endpoint(&[0], 1, 1));
        merge_milestones(&mut aggregate, milestones(&endpoint(&[3], 5, 1)));
        assert_eq!(aggregate.sometimes, 0b1001);
        assert_eq!(aggregate.hooks_finished, 5);
        assert!(!aggregate.bug);
        let bug = FaultObservations {
            stop: FaultStop::Crash,
            ..FaultObservations::default()
        };
        merge_milestones(&mut aggregate, milestones(&bug));
        assert!(aggregate.bug);
        assert_eq!(aggregate.hooks_finished, 5, "a bug never lowers a rung");
        assert!(milestone_key(aggregate) > milestone_key(FaultMilestones::default()));
    }

    #[test]
    fn the_watermark_keeps_the_strongest_of_every_endpoint() {
        let mut watermark = FaultProgressWatermark::default();
        merge_progress_watermark(
            &mut watermark,
            &[endpoint(&[0, 1, 2], 1, 1), endpoint(&[0], 7, 1)],
        );
        assert_eq!(watermark.sometimes_sites, 3);
        assert_eq!(
            watermark.hooks_finished, 1,
            "the watermark is lexicographic"
        );
    }

    fn vocabulary() -> FaultVocabulary {
        FaultVocabulary::new(1, vec![1, 2])
            .expect("vocabulary")
            .with_places(vec![0x4b0e86, 0x47eca0])
            .expect("places")
    }

    #[test]
    fn the_vocabulary_only_draws_declared_nodes_hooks_and_vectors() {
        let vocabulary = vocabulary();
        let mut rand = RomuDuoJrRand::with_seed(11);
        let mut kinds = BTreeSet::new();
        let kind = |action: &FaultAction| match action {
            FaultAction::Wait(_) => 0,
            FaultAction::Kill(_) => 1,
            FaultAction::Pause(..) => 2,
            FaultAction::Restart(_) => 3,
            FaultAction::Hook(_) => 4,
            FaultAction::Interrupt(_) => 5,
            FaultAction::Park { .. } => 6,
            FaultAction::EventKill { .. } => 7,
            FaultAction::EventPark { .. } => 8,
        };
        for _ in 0..2_000 {
            let action = sample_action(&mut rand, &vocabulary, 0).expect("draw an action");
            kinds.insert(kind(&action));
            match action {
                FaultAction::Wait(_) => {}
                FaultAction::EventKill { node, rarity }
                | FaultAction::EventPark { node, rarity, .. } => {
                    assert!(vocabulary.instrumented_events());
                    assert!(node < vocabulary.nodes());
                    assert!(rarity < 64);
                }
                FaultAction::Kill(node) | FaultAction::Restart(node) => {
                    assert!(node < vocabulary.nodes());
                }
                FaultAction::Pause(node, ticks) => {
                    assert!(node < vocabulary.nodes());
                    assert!(PAUSE_TICKS.contains(&ticks));
                }
                FaultAction::Hook(id) => assert!(vocabulary.hooks().contains(&id)),
                FaultAction::Interrupt(vector) => assert!(VECTORS.contains(&vector)),
                FaultAction::Park {
                    node,
                    addr,
                    hits,
                    hold_us,
                } => {
                    assert!(node < vocabulary.nodes());
                    assert!(vocabulary.places().contains(&addr));
                    assert!(PARK_HITS.contains(&hits));
                    assert!(PARK_HOLD_US.contains(&hold_us));
                }
            }
        }
        assert_eq!(
            kinds.len(),
            6 + usize::from(vocabulary.interrupt_injection()),
            "every available action kind is reachable"
        );
    }

    #[test]
    fn event_actions_only_select_nodes_whose_runtime_announced_readiness() {
        let vocabulary = FaultVocabulary::new(3, vec![])
            .unwrap()
            .with_instrumented_events(true);
        let mut rand = RomuDuoJrRand::with_seed(19);
        let mut events = 0;
        for _ in 0..2000 {
            match sample_action(&mut rand, &vocabulary, 0b010).unwrap() {
                FaultAction::EventKill { node, .. } | FaultAction::EventPark { node, .. } => {
                    assert_eq!(node, 1);
                    events += 1;
                }
                _ => {}
            }
            assert!(!matches!(
                sample_action(&mut rand, &vocabulary, 0).unwrap(),
                FaultAction::EventKill { .. } | FaultAction::EventPark { .. }
            ));
        }
        assert!(events > 0);
    }

    #[test]
    fn a_vocabulary_with_no_place_never_draws_a_park() {
        let placeless = FaultVocabulary::new(1, vec![1]).expect("vocabulary");
        let mut rand = RomuDuoJrRand::with_seed(5);
        for _ in 0..2_000 {
            let action = sample_action(&mut rand, &placeless, 0).expect("draw");
            assert!(!matches!(action, FaultAction::Park { .. }));
        }
    }

    #[test]
    fn a_wider_bundle_widens_the_node_range() {
        let wide = FaultVocabulary::new(3, vec![9]).expect("vocabulary");
        let mut rand = RomuDuoJrRand::with_seed(3);
        let mut seen = BTreeSet::new();
        for _ in 0..2_000 {
            if let FaultAction::Kill(node) = sample_action(&mut rand, &wide, 0).expect("draw") {
                seen.insert(node);
            }
        }
        assert_eq!(seen, BTreeSet::from([0, 1, 2]));
    }

    #[test]
    fn a_bundle_with_no_hook_never_draws_one() {
        let hookless = FaultVocabulary::new(1, Vec::new()).expect("vocabulary");
        let mut rand = RomuDuoJrRand::with_seed(4);
        for _ in 0..2_000 {
            let action = sample_action(&mut rand, &hookless, 0).expect("draw");
            assert!(!matches!(action, FaultAction::Hook(_)));
        }
    }

    #[test]
    fn the_vocabulary_is_a_function_of_the_seed() {
        let draw = |seed| {
            let mut rand = RomuDuoJrRand::with_seed(seed);
            (0..64)
                .map(|_| sample_action(&mut rand, &vocabulary(), 0).expect("draw"))
                .collect::<Vec<_>>()
        };
        assert_eq!(draw(5), draw(5));
        assert_ne!(draw(5), draw(6));
    }

    #[test]
    fn the_outcome_counts_the_bugs_and_dates_the_first() {
        assert_eq!(bug_outcome(&[]), (0, None));
        let bug = |execution| FaultBugRecord {
            execution,
            input: FaultInput::default(),
            observations: FaultObservations {
                stop: FaultStop::Crash,
                ..FaultObservations::default()
            },
        };
        assert_eq!(bug_outcome(&[bug(9), bug(4)]), (2, Some(4)));
    }

    #[test]
    fn action_costs_charge_the_recorded_guest_duration() {
        assert_eq!(
            action_cost(&FaultAction::Wait(std::num::NonZeroU16::MIN)),
            1
        );
        assert_eq!(action_cost(&FaultAction::Kill(4)), 50);
        assert_eq!(
            action_cost(&FaultAction::Wait(std::num::NonZeroU16::new(8192).unwrap())),
            8192
        );
    }
}
