// SPDX-License-Identifier: AGPL-3.0-or-later

//! Fault archive key, milestones, and report shape.

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

/// Recorded archive-key policy.
pub const KEY_POLICY_IDENTIFIER: &str = "faultlab_sometimes_hooks_alive_v4_event_deaths";
/// Completed hooks beyond this count stop distinguishing archive cells. A hook
/// the workload lets re-run cheaply, such as a read-back that finds nothing to
/// check, would otherwise turn repetition into an endless supply of new cells
/// and pull the search away from the sites it has not reached.
pub const HOOKS_FINISHED_KEY_CAP: u64 = 8;
/// Recorded same-slot replacement policy.
pub const REPLACEMENT_IDENTIFIER: &str = "fewest_horizons";
/// Recorded action-duration policy: every action costs exactly one horizon.
pub const DURATION_IDENTIFIER: &str = "fixed_horizon_v1";

/// Pooled identity handed to the generic selector.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FaultArchiveGroup {
    sometimes: u64,
    hooks_finished: u64,
    hooks_running: u64,
    alive: u64,
    unexpected_deaths: u64,
    parked: u64,
}

/// Quality-diversity key for one fault-library endpoint: which `sometimes`
/// sites the workload reached, how much of the workload completed, how many
/// hooks are still running, and which nodes are running.
///
/// Hooks in flight are part of the key because the races a fault library
/// exists to find live in the overlap of concurrent activities. A hook that
/// has started and not finished leaves no other trace at its endpoint, so
/// without this count an endpoint with two hooks overlapping pools with one
/// where only the second ever ran, and the search keeps the cheaper of the
/// two.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FaultArchiveKey {
    /// Bitmap of `sometimes` sites hit.
    pub sometimes: u64,
    /// Hooks that ran to completion, saturating at [`HOOKS_FINISHED_KEY_CAP`].
    pub hooks_finished: u64,
    /// Hooks started and not yet finished, saturating at the same cap.
    pub hooks_running: u64,
    /// Bitmap of live nodes.
    pub alive: u64,
    /// Node deaths outside an ordinary kill or restart window, saturating at
    /// [`HOOKS_FINISHED_KEY_CAP`]. An instrumented-event kill is intentionally
    /// observed this way, so a coordinate that actually fired remains a
    /// searchable prefix instead of pooling with an unreachable arm.
    pub unexpected_deaths: u64,
    /// Threads parked at a place, saturating at [`HOOKS_FINISHED_KEY_CAP`].
    /// A park that fired and one that never reached its hit are different
    /// states of the workload.
    pub parked: u64,
}

impl ArchiveKey for FaultArchiveKey {
    type Group = FaultArchiveGroup;

    fn groups() -> usize {
        3
    }

    /// Depth 0 is the whole key, depth 1 drops node liveness so the same
    /// workload progress under different survivor sets pools, and depth 2
    /// keeps reached sites plus event-triggered deaths. Fired event
    /// coordinates must not pool with arms that never reached their ordinal;
    /// that distinction is the evidence used by coordinate refinement.
    fn group(self, depth: usize) -> Self::Group {
        let full = FaultArchiveGroup {
            sometimes: self.sometimes,
            hooks_finished: self.hooks_finished,
            hooks_running: self.hooks_running,
            alive: self.alive,
            unexpected_deaths: self.unexpected_deaths,
            parked: self.parked,
        };
        match depth {
            0 => full,
            1 => FaultArchiveGroup { alive: 0, ..full },
            _ => FaultArchiveGroup {
                sometimes: self.sometimes,
                unexpected_deaths: self.unexpected_deaths,
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

/// The archive key of one endpoint.
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
        unexpected_deaths: observations.unexpected_deaths.min(HOOKS_FINISHED_KEY_CAP),
        parked: observations.parked.min(HOOKS_FINISHED_KEY_CAP),
    }
}

/// Strongest rungs a campaign reached.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultMilestones {
    /// Union of every `sometimes` bitmap observed.
    pub sometimes: u64,
    /// Greatest completed-hook count.
    pub hooks_finished: u64,
    /// Whether any endpoint found a bug.
    pub bug: bool,
}

/// Decode milestones from one endpoint.
#[must_use]
pub fn milestones(observations: &FaultObservations) -> FaultMilestones {
    FaultMilestones {
        sometimes: observations.sometimes_bitmap(),
        hooks_finished: observations.hooks_finished,
        bug: observations.is_bug(),
    }
}

/// Merge the strongest of each rung.
pub fn merge_milestones(into: &mut FaultMilestones, from: FaultMilestones) {
    into.sometimes |= from.sometimes;
    into.hooks_finished = into.hooks_finished.max(from.hooks_finished);
    into.bug |= from.bug;
}

/// Stable champion order: sites reached first, then workload progress.
#[must_use]
pub fn milestone_key(value: FaultMilestones) -> (u32, u64) {
    (value.sometimes.count_ones(), value.hooks_finished)
}

/// Route-agnostic progress watermark.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FaultProgressWatermark {
    /// Greatest number of distinct `sometimes` sites reached.
    pub sometimes_sites: u32,
    /// Greatest completed-hook count.
    pub hooks_finished: u64,
    /// Greatest agent tick count.
    pub ticks: u64,
}

/// Fold one endpoint's observations into a progress watermark.
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

/// Every action costs exactly one horizon, so route cost is action count.
#[must_use]
pub fn action_time(_action: &FaultAction) -> u64 {
    1
}

/// Pause durations in agent ticks.
pub const PAUSE_TICKS: [u32; 4] = [1, 5, 25, 100];
/// Interrupt vectors the vocabulary may inject.
pub const VECTORS: [u32; 2] = [0x20, 0x30];
/// The hit a park waits for. A place is reached a handful of times or
/// thousands of times per horizon, so the ladder spans both.
pub const PARK_HITS: [u32; 8] = [1, 2, 4, 8, 16, 32, 64, 128];
/// Lengths of a park's hold, in microseconds. The guest kernel releases the
/// thread at the first system call of another task past the deadline, or at
/// the periodic tick, so a short hold lands within a few milliseconds.
pub const PARK_HOLD_US: [u32; 3] = [500, 2_000, 10_000];

/// Draw one action from the bundle's vocabulary. A bundle that declares no
/// hook cannot draw one, so that arm yields `Wait` rather than an action the
/// guest agent would skip.
///
/// # Errors
///
/// Returns an error when a draw bound is invalid.
pub fn sample_action(
    rand: &mut RomuDuoJrRand,
    vocabulary: &FaultVocabulary,
) -> Result<FaultAction, Box<dyn Error>> {
    let pick = |rand: &mut RomuDuoJrRand, len: usize| -> Result<usize, Box<dyn Error>> {
        Ok(rand.below(NonZeroUsize::new(len).ok_or("empty fault vocabulary alternative")?))
    };
    let node = u16::try_from(pick(rand, usize::from(vocabulary.nodes()))?)?;
    match pick(rand, 8)? {
        0 => Ok(FaultAction::Wait),
        1 if vocabulary.instrumented_events() => Ok(FaultAction::EventKill {
            node,
            ordinal: sample_event_ordinal(rand),
        }),
        1 => Ok(FaultAction::Wait),
        2 => Ok(FaultAction::Kill(node)),
        3 => Ok(FaultAction::Pause(
            node,
            PAUSE_TICKS[pick(rand, PAUSE_TICKS.len())?],
        )),
        4 => Ok(FaultAction::Restart(node)),
        5 => match vocabulary.hooks() {
            [] => Ok(FaultAction::Wait),
            hooks => Ok(FaultAction::Hook(hooks[pick(rand, hooks.len())?])),
        },
        6 if vocabulary.interrupt_injection() => {
            Ok(FaultAction::Interrupt(VECTORS[pick(rand, VECTORS.len())?]))
        }
        // The arm64 Consonance backend delegates the GIC to KVM, so it has no
        // generic host interrupt injection seam. Keep the draw deterministic
        // while replacing the unavailable arm with a supported no-op action.
        6 => Ok(FaultAction::Wait),
        _ => match vocabulary.places() {
            [] => Ok(FaultAction::Wait),
            places => Ok(FaultAction::Park {
                node,
                addr: places[pick(rand, places.len())?],
                hits: PARK_HITS[pick(rand, PARK_HITS.len())?],
                hold_us: PARK_HOLD_US[pick(rand, PARK_HOLD_US.len())?],
            }),
        },
    }
}

/// Draw across every positive `u64` scale without importing a workload event
/// bound. Uniform raw integers almost never land in the finite event prefix an
/// action executes. Choosing the binary scale uniformly gives short and long
/// actions equal access to reachable coordinates while the low bits select a
/// point within that scale.
fn sample_event_ordinal(rand: &mut RomuDuoJrRand) -> u64 {
    event_ordinal_from_word(rand.next_u64())
}

fn event_ordinal_from_word(word: u64) -> u64 {
    let exponent = (word >> 58) as u32;
    (1_u64 << exponent) | (word & ((1_u64 << exponent).wrapping_sub(1)))
}

/// Progress-curve point.
pub type FaultProgressPoint = ProgressPoint<FaultMilestones, FaultProgressWatermark>;
/// Archive entry report.
pub type FaultArchiveEntryReport =
    ArchiveEntryReport<FaultAction, FaultArchiveKey, FaultMilestones>;
/// Search input.
pub type FaultInput = searcher::search::archive::Input<FaultAction>;

/// Largest number of bugs one campaign records in its report, so a run whose
/// workload asserts on every branch still holds bounded memory.
pub const MAX_RECORDED_BUGS: usize = 64;

/// One bug an execution found.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultBugRecord {
    /// Ordered admission position of the execution that found it.
    pub execution: u64,
    /// The input that reaches it.
    pub input: FaultInput,
    /// The terminal endpoint's observations.
    pub observations: FaultObservations,
}

/// Observation-only coordinate-refinement counters. These are reported for
/// campaign diagnostics and never feed archive selection or admission.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultCoordinateTelemetry {
    /// EventKill observations admitted, fired or unfired.
    pub attempted: u64,
    /// EventKill observations whose arm fired.
    pub fired: u64,
    /// Redraw requests actually dispatched by the coordinator.
    pub refined: u64,
}

/// The campaign outcome a bug list implies: how many bugs were found and the
/// admission position of the first.
#[must_use]
pub fn bug_outcome(bugs: &[FaultBugRecord]) -> (u64, Option<u64>) {
    (
        bugs.len() as u64,
        bugs.iter().map(|bug| bug.execution).min(),
    )
}

/// Complete deterministic report for one fault campaign.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultArchiveReport {
    /// Campaign seed.
    pub seed: u64,
    /// The sealed setup `Moment` every action window is measured from; zero
    /// when the run never booted a target.
    pub root_seal: u64,
    /// Virtual nanoseconds each action window spans.
    pub horizon_nanos: u64,
    /// Admitted executions.
    pub executions: u64,
    /// Strongest milestones.
    pub milestones: FaultMilestones,
    /// Strongest route-agnostic progress.
    pub progress_watermark: FaultProgressWatermark,
    /// Best input under the adapter's progress order.
    pub champion_input: FaultInput,
    /// Retained representatives.
    #[serde(with = "entries_by_suffix")]
    pub entries: Vec<FaultArchiveEntryReport>,
    /// Fixed-interval progress curve.
    pub progress_curve: Vec<FaultProgressPoint>,
    /// Candidates admitted.
    pub retained: u64,
    /// Candidates rejected or superseded.
    pub rejected: u64,
    /// Terminal endpoints observed.
    pub deaths: u64,
    /// Bugs found, in admission order, bounded by [`MAX_RECORDED_BUGS`].
    pub bugs: Vec<FaultBugRecord>,
    /// Non-decision EventKill refinement telemetry.
    #[serde(default)]
    pub coordinate_telemetry: FaultCoordinateTelemetry,
    /// Generic selector accounting.
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
                unexpected_deaths: 0,
                parked: 0,
            }
        );
    }

    #[test]
    fn unexpected_deaths_keep_a_fired_event_coordinate_in_its_own_cell() {
        let armed_but_unreached = endpoint(&[1], 0, 1);
        let mut fired = armed_but_unreached.clone();
        fired.unexpected_deaths = 1;
        assert_ne!(archive_key(&armed_but_unreached), archive_key(&fired));
        assert_eq!(archive_key(&fired).unexpected_deaths, 1);
        assert_ne!(
            archive_key(&armed_but_unreached).group(2),
            archive_key(&fired).group(2),
        );

        fired.unexpected_deaths = HOOKS_FINISHED_KEY_CAP + 1;
        assert_eq!(
            archive_key(&fired).unexpected_deaths,
            HOOKS_FINISHED_KEY_CAP
        );
    }

    #[test]
    fn event_coordinate_draw_is_uniform_over_binary_scales() {
        assert_eq!(event_ordinal_from_word(0), 1);
        assert_eq!(event_ordinal_from_word(10_u64 << 58), 1 << 10);
        assert_eq!(
            event_ordinal_from_word((10_u64 << 58) | 123),
            (1 << 10) | 123
        );
        assert_eq!(event_ordinal_from_word(u64::MAX), u64::MAX);
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
            .with_instrumented_events(true)
            .with_places(vec![0x4b0e86, 0x47eca0])
            .expect("places")
    }

    #[test]
    fn the_vocabulary_only_draws_declared_nodes_hooks_and_vectors() {
        let vocabulary = vocabulary();
        let mut rand = RomuDuoJrRand::with_seed(11);
        let mut kinds = BTreeSet::new();
        let kind = |action: &FaultAction| match action {
            FaultAction::Wait => 0,
            FaultAction::EventKill { .. } => 1,
            FaultAction::Kill(_) => 2,
            FaultAction::Pause(..) => 3,
            FaultAction::Restart(_) => 4,
            FaultAction::Hook(_) => 5,
            FaultAction::Interrupt(_) => 6,
            FaultAction::Park { .. } => 7,
        };
        for _ in 0..2_000 {
            let action = sample_action(&mut rand, &vocabulary).expect("draw an action");
            kinds.insert(kind(&action));
            match action {
                FaultAction::Wait => {}
                FaultAction::EventKill { node, ordinal } => {
                    assert!(node < vocabulary.nodes());
                    assert!(ordinal > 0);
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
        let expected = if vocabulary.interrupt_injection() {
            8
        } else {
            7
        };
        assert_eq!(
            kinds.len(),
            expected,
            "every supported action kind is reachable"
        );
    }

    #[test]
    fn a_vocabulary_with_no_place_never_draws_a_park() {
        let placeless = FaultVocabulary::new(1, vec![1]).expect("vocabulary");
        let mut rand = RomuDuoJrRand::with_seed(5);
        for _ in 0..2_000 {
            let action = sample_action(&mut rand, &placeless).expect("draw");
            assert!(!matches!(action, FaultAction::Park { .. }));
        }
    }

    #[test]
    fn an_uninstrumented_vocabulary_never_draws_an_event_kill() {
        let uninstrumented = FaultVocabulary::new(1, vec![1]).expect("vocabulary");
        let mut rand = RomuDuoJrRand::with_seed(5);
        for _ in 0..2_000 {
            let action = sample_action(&mut rand, &uninstrumented).expect("draw");
            assert!(!matches!(action, FaultAction::EventKill { .. }));
        }
    }

    #[test]
    fn a_wider_bundle_widens_the_node_range() {
        let wide = FaultVocabulary::new(3, vec![9]).expect("vocabulary");
        let mut rand = RomuDuoJrRand::with_seed(3);
        let mut seen = BTreeSet::new();
        for _ in 0..2_000 {
            if let FaultAction::Kill(node) = sample_action(&mut rand, &wide).expect("draw") {
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
            let action = sample_action(&mut rand, &hookless).expect("draw");
            assert!(!matches!(action, FaultAction::Hook(_)));
        }
    }

    #[test]
    fn the_vocabulary_is_a_function_of_the_seed() {
        let draw = |seed| {
            let mut rand = RomuDuoJrRand::with_seed(seed);
            (0..64)
                .map(|_| sample_action(&mut rand, &vocabulary()).expect("draw"))
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
    fn every_action_costs_one_horizon() {
        assert_eq!(action_time(&FaultAction::Wait), 1);
        assert_eq!(action_time(&FaultAction::Kill(4)), 1);
    }
}
