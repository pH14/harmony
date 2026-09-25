// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    error::Error,
    num::{NonZeroU16, NonZeroUsize},
};

use serde::{Deserialize, Serialize};

use fault_policy::ParkTarget;
use searcher::search::{
    archive::{
        ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting, entries_by_suffix,
    },
    draw_tables::DrawView,
    rand::RomuDuoJrRand,
};

use crate::assertion::{AssertionSet, Assertions};
use crate::bundle::FaultVocabulary;
use crate::target::{FaultAction, FaultObservations, SUPERVISOR_TICK_MICROS};

pub use searcher::search::archive::MAX_ARCHIVE_ENTRIES;

pub const KEY_POLICY_IDENTIFIER: &str = "faultlab_goals_reached_tiers_assertion_ids_held_park_armed_kill_places_liveness_edge_buckets_identity_v11";
pub const HOOKS_FINISHED_KEY_CAP: u64 = 8;
pub const REPLACEMENT_IDENTIFIER: &str = "fewest_guest_ticks";
pub const DURATION_IDENTIFIER: &str = "adaptive_action_ticks_v3";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FaultArchiveKey {
    pub sometimes: AssertionSet,
    pub hooks_finished: u64,
    pub hooks_running: u64,
    pub alive: u64,
    pub event_ready: u64,
    pub event_kill_fires: u64,
    pub event_park_fires: u64,
    pub checks_finished: u64,
    pub checks_running: bool,
    pub workload_running: bool,
    pub park_held: bool,
    pub kill_armed: bool,
    pub edges: u64,
}

impl ArchiveKey for FaultArchiveKey {
    type Place = (
        AssertionSet,
        u64,
        u64,
        u64,
        u64,
        u64,
        u64,
        bool,
        bool,
        bool,
        bool,
    );
    type Progress = u32;
    type Identity = (u64, u64);

    fn place(self) -> Self::Place {
        (
            self.sometimes,
            self.hooks_finished,
            self.hooks_running,
            self.event_ready,
            self.event_kill_fires,
            self.event_park_fires,
            self.checks_finished,
            self.checks_running,
            self.workload_running,
            self.park_held,
            self.kill_armed,
        )
    }

    fn progress(self) -> Self::Progress {
        self.sometimes.count
    }

    fn identity(self) -> Self::Identity {
        (self.alive, self.edges)
    }

    fn capacity() -> usize {
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
        sometimes: observations.assertions.key(),
        hooks_finished: observations.hooks_finished.min(HOOKS_FINISHED_KEY_CAP),
        hooks_running: observations
            .hooks_started
            .saturating_sub(observations.hooks_finished)
            .min(HOOKS_FINISHED_KEY_CAP),
        alive: observations.alive,
        event_ready: observations.event_ready,
        event_kill_fires: observations.event_kill_fires.min(HOOKS_FINISHED_KEY_CAP),
        event_park_fires: observations.event_park_fires.min(HOOKS_FINISHED_KEY_CAP),
        checks_finished: observations.checks_finished.min(HOOKS_FINISHED_KEY_CAP),
        checks_running: observations.checks_started > observations.checks_finished,
        workload_running: observations.workload_started > observations.workload_finished,
        park_held: observations.event_park_held != 0,
        kill_armed: observations.event_kill_armed != 0,
        edges: observations.edge_digest,
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultMilestones {
    pub sometimes: u32,
    pub hooks_finished: u64,
    pub bug: bool,
}

#[must_use]
pub fn milestones(observations: &FaultObservations) -> FaultMilestones {
    FaultMilestones {
        sometimes: observations.assertions.key().count,
        hooks_finished: observations.hooks_finished,
        bug: observations.is_bug(),
    }
}

pub fn merge_milestones(into: &mut FaultMilestones, from: FaultMilestones) {
    into.sometimes = into.sometimes.max(from.sometimes);
    into.hooks_finished = into.hooks_finished.max(from.hooks_finished);
    into.bug |= from.bug;
}

#[must_use]
pub fn milestone_key(value: FaultMilestones) -> (u32, u64) {
    (value.sometimes, value.hooks_finished)
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
            sometimes_sites: observation.assertions.key().count,
            hooks_finished: observation.hooks_finished,
            ticks: observation.ticks,
        });
    }
}

#[must_use]
pub fn action_cost(action: &FaultAction) -> u64 {
    crate::target::action_ticks(action)
}

pub const VECTORS: [u32; 2] = [0x20, 0x30];

pub fn sample_action(
    rand: &mut RomuDuoJrRand,
    vocabulary: &FaultVocabulary,
    event_ready: u64,
    ticks: NonZeroU16,
    view: Option<DrawView<'_, FaultAction>>,
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
    let node_mask = u64::MAX >> (64 - vocabulary.nodes());
    let event_ready = event_ready & node_mask;
    if vocabulary.instrumented_events() && event_ready != 0 {
        alternatives.extend([6, 7]);
    }
    let event_node = |rand: &mut RomuDuoJrRand| -> Result<u16, Box<dyn Error>> {
        let index = pick(rand, event_ready.count_ones() as usize)?;
        let mut bits = event_ready;
        for _ in 0..index {
            bits &= bits - 1;
        }
        Ok(u16::try_from(bits.trailing_zeros())?)
    };
    let action = match alternatives[pick(rand, alternatives.len())?] {
        0 => FaultAction::Wait(ticks),
        1 => FaultAction::Kill(node, ticks),
        2 => FaultAction::Pause(node, ticks),
        3 => FaultAction::Restart(node, ticks),
        4 => FaultAction::Hook(
            vocabulary.hooks()[pick(rand, vocabulary.hooks().len())?],
            ticks,
        ),
        5 => FaultAction::Interrupt(VECTORS[pick(rand, VECTORS.len())?], ticks),
        6 => FaultAction::EventKill {
            node: event_node(rand)?,
            rarity: u8::try_from(pick(rand, 64)?)?,
            ticks,
        },
        _ => {
            let node = event_node(rand)?;
            let target = match view {
                Some(view) => park_target(rand, view)?,
                None => None,
            };
            FaultAction::EventPark {
                node,
                edges: if target.is_some() {
                    1
                } else {
                    park_edges(rand)?
                },
                hold_us: park_hold_us(rand, ticks)?,
                ticks,
                target,
            }
        }
    };
    Ok(action)
}

const PARK_EDGE_EXPONENTS: u32 = 14;

const PARK_TARGET_LEVELS: usize = 8;

const PARK_TARGET_RADIUS_SHIFT: u32 = 5;

fn park_target(
    rand: &mut RomuDuoJrRand,
    view: DrawView<'_, FaultAction>,
) -> Result<Option<ParkTarget>, Box<dyn Error>> {
    if view.feedback.is_empty() || rand.below(NonZeroUsize::MIN.saturating_add(1)) == 0 {
        return Ok(None);
    }
    let Some(site) = view.pick_feedback(rand)? else {
        return Ok(None);
    };
    let level = u32::try_from(
        rand.below(NonZeroUsize::new(PARK_TARGET_LEVELS).ok_or("empty park target levels")?),
    )?;
    let radius = if level == 0 {
        0
    } else {
        1_u64 << (level + PARK_TARGET_RADIUS_SHIFT)
    };
    Ok(ParkTarget::new(
        site.saturating_sub(radius),
        site.saturating_add(radius).saturating_add(1),
    ))
}

fn park_edges(rand: &mut RomuDuoJrRand) -> Result<u32, Box<dyn Error>> {
    let exponent = u32::try_from(
        rand.below(
            NonZeroUsize::new(usize::try_from(PARK_EDGE_EXPONENTS)?)
                .ok_or("empty edge exponent range")?,
        ),
    )?;
    let low = 1_u32 << exponent;
    let offset = u32::try_from(
        rand.below(NonZeroUsize::new(usize::try_from(low)?).ok_or("empty edge range")?),
    )?;
    Ok(low + offset)
}

fn park_hold_us(rand: &mut RomuDuoJrRand, ticks: NonZeroU16) -> Result<u32, Box<dyn Error>> {
    let window = u32::from(ticks.get());
    let exponent = u32::try_from(
        rand.below(
            NonZeroUsize::new(usize::try_from(window.ilog2() + 1)?)
                .ok_or("empty hold exponent range")?,
        ),
    )?;
    let low = 1_u32 << exponent;
    let high = window.min((low << 1) - 1);
    let offset = u32::try_from(
        rand.below(NonZeroUsize::new(usize::try_from(high - low + 1)?).ok_or("empty hold range")?),
    )?;
    Ok((low + offset).saturating_mul(u32::try_from(SUPERVISOR_TICK_MICROS)?))
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
    #[serde(default)]
    pub assertions: Assertions,
    #[serde(default)]
    pub park_sites: BTreeMap<u64, u64>,
    #[serde(default)]
    pub park_reads: BTreeMap<u64, u64>,
    #[serde(default)]
    pub park_thresholds: BTreeMap<u32, ParkThresholds>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ParkThresholds {
    pub armed: u64,
    pub fired: u64,
}

#[must_use]
pub fn park_threshold_bucket(edges: u64) -> u32 {
    edges.max(1).ilog2()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::assertion::{AssertionKind, AssertionOutcome};
    use crate::target::FaultStop;
    use searcher::search::{
        draw_tables::{DEFAULT_DRAW_TABLE_PARAMETERS, DrawTables},
        empirical_steps::EmpiricalStepParameters,
    };

    fn endpoint(sometimes: &[u32], hooks_finished: u64, alive: u64) -> FaultObservations {
        let mut assertions = Assertions::default();
        for id in sometimes {
            assertions.record(
                id.to_string(),
                AssertionOutcome {
                    kind: AssertionKind::Sometimes,
                    message: id.to_string(),
                    location: String::new(),
                    passed: true,
                    failed: false,
                },
            );
        }
        FaultObservations {
            assertions,
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
                sometimes: AssertionSet::of(["0", "5"]),
                hooks_finished: 3,
                hooks_running: 0,
                alive: 0b11,
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
    fn an_in_flight_check_opens_its_own_cell() {
        let completed = archive_key(&FaultObservations {
            checks_started: 1,
            checks_finished: 1,
            ..FaultObservations::default()
        });
        let running = archive_key(&FaultObservations {
            checks_started: 2,
            checks_finished: 1,
            ..FaultObservations::default()
        });
        assert_ne!(completed, running);
        assert!(!completed.checks_running);
        assert!(running.checks_running);
    }

    #[test]
    fn a_held_park_thread_and_an_armed_event_kill_each_open_their_own_place() {
        let quiet = FaultObservations {
            alive: 0b11,
            ..FaultObservations::default()
        };
        let held = FaultObservations {
            event_park_held: 0b10,
            ..quiet.clone()
        };
        let armed = FaultObservations {
            event_kill_armed: 0b01,
            ..quiet.clone()
        };
        let places = [&quiet, &held, &armed]
            .map(|observations| archive_key(observations).place())
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(places.len(), 3);
        assert!(archive_key(&held).park_held);
        assert!(archive_key(&armed).kill_armed);
        let other_node = FaultObservations {
            event_park_held: 0b01,
            ..quiet
        };
        assert_eq!(archive_key(&other_node), archive_key(&held));
    }

    #[test]
    fn liveness_is_the_holder_identity_inside_one_place() {
        let survived = archive_key(&endpoint(&[1], 2, 0b11));
        let lost_a_node = archive_key(&endpoint(&[1], 2, 0b01));
        assert_ne!(survived.identity(), lost_a_node.identity());
        assert_eq!(survived.place(), lost_a_node.place());
        assert_eq!(FaultArchiveKey::capacity(), 1);
    }

    #[test]
    fn park_thresholds_bucket_by_power_of_two() {
        assert_eq!(park_threshold_bucket(0), 0);
        assert_eq!(park_threshold_bucket(1), 0);
        assert_eq!(park_threshold_bucket(3), 1);
        assert_eq!(park_threshold_bucket(4), 2);
        assert_eq!(
            park_threshold_bucket(u64::from(fault_policy::EVENT_PARK_EDGE_LIMIT)),
            24
        );
    }

    #[test]
    fn edge_bucket_coverage_separates_identities_inside_one_place() {
        let before = archive_key(&endpoint(&[1], 2, 0b11));
        let crossed = archive_key(&FaultObservations {
            edge_crossings: 3,
            edge_digest: 0x5eed,
            ..endpoint(&[1], 2, 0b11)
        });
        assert_eq!(crossed.edges, 0x5eed);
        assert_eq!(before.place(), crossed.place());
        assert_ne!(before.identity(), crossed.identity());
    }

    #[test]
    fn hook_progress_separates_places_inside_one_tier() {
        let early = archive_key(&endpoint(&[1], 1, 0b1));
        let late = archive_key(&endpoint(&[1], 4, 0b1));
        assert_eq!(early.progress(), late.progress());
        assert_ne!(early.place(), late.place());
    }

    #[test]
    fn the_goals_reached_are_the_progress_tier() {
        let none = archive_key(&endpoint(&[], 0, 0b1));
        let one = archive_key(&endpoint(&[4], 0, 0b1));
        let other_one = archive_key(&endpoint(&[7], 0, 0b1));
        let two = archive_key(&endpoint(&[4, 7], 0, 0b1));
        assert_eq!(none.progress(), 0);
        assert_eq!(one.progress(), 1);
        assert_eq!(two.progress(), 2);
        assert_eq!(one.progress(), other_one.progress());
        assert_ne!(one.place(), other_one.place());
        assert!(two.progress() > one.progress());
    }

    #[test]
    fn milestones_union_the_sites_and_latch_the_bug() {
        let mut aggregate = milestones(&endpoint(&[0], 1, 1));
        merge_milestones(&mut aggregate, milestones(&endpoint(&[3], 5, 1)));
        assert_eq!(aggregate.sometimes, 1);
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

    const TICKS: NonZeroU16 = NonZeroU16::new(50).unwrap();

    fn vocabulary() -> FaultVocabulary {
        FaultVocabulary::new(1, vec![1, 2]).expect("vocabulary")
    }

    #[test]
    fn the_vocabulary_only_draws_declared_nodes_hooks_and_vectors() {
        let vocabulary = vocabulary();
        let mut rand = RomuDuoJrRand::with_seed(11);
        let mut kinds = BTreeSet::new();
        let kind = |action: &FaultAction| match action {
            FaultAction::Wait(_) => 0,
            FaultAction::Kill(..) => 1,
            FaultAction::Pause(..) => 2,
            FaultAction::Restart(..) => 3,
            FaultAction::Hook(..) => 4,
            FaultAction::Interrupt(..) => 5,
            FaultAction::EventKill { .. } => 6,
            FaultAction::EventPark { .. } => 7,
        };
        for _ in 0..2_000 {
            let action =
                sample_action(&mut rand, &vocabulary, 0, TICKS, None).expect("draw an action");
            kinds.insert(kind(&action));
            assert_eq!(action.ticks(), u64::from(TICKS.get()));
            match action {
                FaultAction::Wait(_) => {}
                FaultAction::EventKill { node, rarity, .. } => {
                    assert!(vocabulary.instrumented_events());
                    assert!(node < vocabulary.nodes());
                    assert!(rarity < 64);
                }
                FaultAction::EventPark {
                    node,
                    edges,
                    hold_us,
                    ..
                } => {
                    assert!(vocabulary.instrumented_events());
                    assert!(node < vocabulary.nodes());
                    assert!((1..1 << PARK_EDGE_EXPONENTS).contains(&edges));
                    assert!(
                        (SUPERVISOR_TICK_MICROS..=u64::from(TICKS.get()) * SUPERVISOR_TICK_MICROS)
                            .contains(&u64::from(hold_us))
                    );
                }
                FaultAction::Kill(node, _)
                | FaultAction::Restart(node, _)
                | FaultAction::Pause(node, _) => {
                    assert!(node < vocabulary.nodes());
                }
                FaultAction::Hook(id, _) => assert!(vocabulary.hooks().contains(&id)),
                FaultAction::Interrupt(vector, _) => assert!(VECTORS.contains(&vector)),
            }
        }
        assert_eq!(
            kinds.len(),
            5 + usize::from(vocabulary.interrupt_injection()),
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
            match sample_action(&mut rand, &vocabulary, 0b010, TICKS, None).unwrap() {
                FaultAction::EventKill { node, .. } | FaultAction::EventPark { node, .. } => {
                    assert_eq!(node, 1);
                    events += 1;
                }
                _ => {}
            }
            assert!(!matches!(
                sample_action(&mut rand, &vocabulary, 0, TICKS, None).unwrap(),
                FaultAction::EventKill { .. } | FaultAction::EventPark { .. }
            ));
        }
        assert!(events > 0);
    }

    #[test]
    fn half_the_parks_aim_at_a_weighted_site_once_feedback_exists() {
        let vocabulary = FaultVocabulary::new(1, Vec::new())
            .expect("vocabulary")
            .with_instrumented_events(true);
        let mut tables = DrawTables::<FaultAction>::new(EmpiricalStepParameters {
            update_every_records: 1,
            ..DEFAULT_DRAW_TABLE_PARAMETERS
        })
        .expect("draw tables");
        let parks = |tables: &DrawTables<FaultAction>| {
            let mut rand = RomuDuoJrRand::with_seed(31);
            tables
                .draw(None, false, |view| {
                    Ok((0..4_000)
                        .map(|_| sample_action(&mut rand, &vocabulary, 1, TICKS, Some(view)))
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .filter_map(|action| match action {
                            FaultAction::EventPark { edges, target, .. } => Some((edges, target)),
                            _ => None,
                        })
                        .collect::<Vec<_>>())
                })
                .expect("draw parks")
        };
        assert!(parks(&tables).iter().all(|(_, target)| target.is_none()));
        tables
            .finish_record_with_feedback(&[], || BTreeMap::from([(0x8000, 1), (0x20, 1)]))
            .expect("set feedback");
        let drawn = parks(&tables);
        let aimed = drawn
            .iter()
            .filter_map(|(edges, target)| target.map(|target| (*edges, target)))
            .collect::<Vec<_>>();
        assert!(aimed.len() * 3 > drawn.len() && aimed.len() * 3 < drawn.len() * 2);
        let mut widths = BTreeSet::new();
        for (edges, target) in aimed {
            assert_eq!(edges, 1);
            let width = target.end - target.start;
            let site = if target.start <= 0x20 && 0x20 < target.end {
                0x20
            } else {
                0x8000
            };
            assert!(target.start <= site && site < target.end);
            if site == 0x8000 {
                assert_eq!(width % 2, 1);
                assert_eq!(site - target.start, target.end - 1 - site);
                widths.insert(width);
            }
        }
        assert_eq!(
            widths,
            [1, 129, 257, 513, 1025, 2049, 4097, 8193]
                .into_iter()
                .collect::<BTreeSet<_>>()
        );
    }

    #[test]
    fn park_edge_counts_cover_every_power_of_two_below_the_draw_limit() {
        let mut rand = RomuDuoJrRand::with_seed(23);
        let mut exponents = BTreeSet::new();
        for _ in 0..5_000 {
            let edges = park_edges(&mut rand).unwrap();
            assert!((1..1 << PARK_EDGE_EXPONENTS).contains(&edges));
            exponents.insert(edges.ilog2());
        }
        assert_eq!(exponents, (0..PARK_EDGE_EXPONENTS).collect::<BTreeSet<_>>());
    }

    #[test]
    fn park_holds_cover_every_power_of_two_tick_count_up_to_the_window() {
        let mut rand = RomuDuoJrRand::with_seed(29);
        let mut exponents = BTreeSet::new();
        for _ in 0..5_000 {
            let hold = u64::from(park_hold_us(&mut rand, TICKS).unwrap());
            assert!(hold.is_multiple_of(SUPERVISOR_TICK_MICROS));
            let hold_ticks = hold / SUPERVISOR_TICK_MICROS;
            assert!((1..=u64::from(TICKS.get())).contains(&hold_ticks));
            exponents.insert(hold_ticks.ilog2());
        }
        assert_eq!(
            exponents,
            (0..=u32::from(TICKS.get()).ilog2()).collect::<BTreeSet<_>>()
        );
        let one = NonZeroU16::MIN;
        assert_eq!(
            park_hold_us(&mut rand, one).unwrap(),
            u32::try_from(SUPERVISOR_TICK_MICROS).unwrap()
        );
    }

    #[test]
    fn a_wider_bundle_widens_the_node_range() {
        let wide = FaultVocabulary::new(3, vec![9]).expect("vocabulary");
        let mut rand = RomuDuoJrRand::with_seed(3);
        let mut seen = BTreeSet::new();
        for _ in 0..2_000 {
            if let FaultAction::Kill(node, _) =
                sample_action(&mut rand, &wide, 0, TICKS, None).expect("draw")
            {
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
            let action = sample_action(&mut rand, &hookless, 0, TICKS, None).expect("draw");
            assert!(!matches!(action, FaultAction::Hook(..)));
        }
    }

    #[test]
    fn the_vocabulary_is_a_function_of_the_seed() {
        let draw = |seed| {
            let mut rand = RomuDuoJrRand::with_seed(seed);
            (0..64)
                .map(|_| sample_action(&mut rand, &vocabulary(), 0, TICKS, None).expect("draw"))
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
        assert_eq!(action_cost(&FaultAction::Kill(4, TICKS)), 50);
        assert_eq!(
            action_cost(&FaultAction::Wait(std::num::NonZeroU16::new(8192).unwrap())),
            8192
        );
    }
}
