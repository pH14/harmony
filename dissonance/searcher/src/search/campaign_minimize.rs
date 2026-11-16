// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeSet, VecDeque},
    error::Error,
    num::NonZeroU64,
};

use serde::{Deserialize, Serialize};

use super::{CampaignAdmissionDecision, CampaignIncumbentRecord, CoordinatorCore, Workload};
use crate::search::{
    archive::{Archive, slot_of_key},
    chain::Chain,
    cost::ShrinkCost,
    draw::MixtureEnergy,
    rand::RomuDuoJrRand,
    shortcut::Schedule,
    shrink::{CONTINUATION_QUEUE_LIMIT, JobKind, KindCounts, ShrinkStop},
};

const KIND_DRAW_SALT: u64 = 0x6b69_6e64_5f64_7261;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MinimizeConfig {
    pub energy_scale: u64,
    pub ordinary: bool,
    pub primary_shortcuts: bool,
    pub shortcut_snapshots: bool,
    pub until_shortcuts_exhausted: bool,
    #[serde(default)]
    pub ordinary_length: Option<u8>,
}

impl Default for MinimizeConfig {
    fn default() -> Self {
        Self {
            energy_scale: 6,
            ordinary: true,
            primary_shortcuts: false,
            shortcut_snapshots: false,
            until_shortcuts_exhausted: false,
            ordinary_length: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShortcutRecord {
    pub start: usize,
    pub position: usize,
    pub size: usize,
    pub variant: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContinuationRecord {
    pub entry: u64,
    pub index: usize,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ShrinkSelectionRecord {
    pub kind: JobKind,
    pub shares: [u64; 3],
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<ShortcutRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ContinuationRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MinimizeReport {
    pub jobs: KindCounts,
    pub accepted: KindCounts,
    pub shares: [u64; 3],
    pub generation: u64,
    pub pending_continuations: usize,
    pub shortcut_window: usize,
    pub shortcut_position: usize,
    pub floor: ShrinkCost,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ShrinkState {
    pub(crate) config: MinimizeConfig,
    floor: ShrinkCost,
    generation: u64,
    schedule: Schedule,
    queue: VecDeque<(u64, usize)>,
    proposed: BTreeSet<(u64, usize)>,
    energy: MixtureEnergy,
    jobs: KindCounts,
    accepted: KindCounts,
    pub(crate) stop: Option<ShrinkStop>,
}

pub(crate) struct ChainJob<A> {
    pub(crate) record: ShrinkSelectionRecord,
    pub(crate) parent_index: usize,
    pub(crate) suffix: Vec<A>,
    pub(crate) candidates: bool,
}

pub(crate) enum ShrinkReservation<A> {
    Unused,
    Stopped,
    Ordinary(ShrinkSelectionRecord),
    Chain(ChainJob<A>),
}

impl ShrinkState {
    pub(crate) fn new(config: MinimizeConfig) -> Self {
        Self {
            config,
            floor: ShrinkCost::ZERO,
            generation: 0,
            schedule: Schedule::default(),
            queue: VecDeque::new(),
            proposed: BTreeSet::new(),
            energy: MixtureEnergy::default(),
            jobs: KindCounts::default(),
            accepted: KindCounts::default(),
            stop: None,
        }
    }

    fn shares(&self) -> [u64; 3] {
        let mut shares = self.energy.shares(self.config.energy_scale.max(1));
        if !self.config.ordinary {
            shares[JobKind::Ordinary.index()] = 0;
        }
        shares
    }

    pub(crate) fn report(&self) -> MinimizeReport {
        MinimizeReport {
            jobs: self.jobs,
            accepted: self.accepted,
            shares: self.shares(),
            generation: self.generation,
            pending_continuations: self.queue.len(),
            shortcut_window: self.schedule.size,
            shortcut_position: self.schedule.position,
            floor: self.floor,
        }
    }
}

type CoreArchive<G> = Archive<
    <G as super::CampaignTypes>::Action,
    <G as super::CampaignTypes>::Key,
    <G as super::CampaignTypes>::Milestones,
    <G as super::CampaignTypes>::Snapshot,
>;

pub(crate) fn selectable<G: Workload + ?Sized>(
    archive: &CoreArchive<G>,
    root: usize,
    incumbent: Option<ShrinkCost>,
    index: usize,
) -> bool {
    let Some(entry) = archive.entries.get(index) else {
        return false;
    };
    if entry.input_len < root {
        return false;
    }
    incumbent.is_none_or(|incumbent| {
        archive
            .entry_origin_cost(index)
            .zip(archive.entry_remaining_bound(index))
            .is_some_and(|(cost, bound)| cost + bound < incumbent)
    })
}

fn eligible<G: Workload + ?Sized>(chain: &Chain<G>, root: usize, primary: bool) -> Vec<usize> {
    (root..chain.actions.len())
        .filter(|&index| !primary || chain.costs[index + 1].primary > chain.costs[index].primary)
        .collect()
}

fn job_origin<G: Workload + ?Sized>(archive: &CoreArchive<G>, id: Option<u64>) -> Option<usize> {
    id.and_then(|id| archive.index_of_id(id))
        .filter(|&index| archive.active[index] && archive.job_origin_resident(index))
}

impl<G: Workload + ?Sized> CoordinatorCore<G> {
    pub(crate) fn set_minimize_floor(&mut self, floor: ShrinkCost) {
        if let Some(state) = self.shrink.as_mut() {
            state.floor = floor;
        }
    }

    pub(crate) fn minimize_report(&self) -> Option<MinimizeReport> {
        self.shrink.as_ref().map(ShrinkState::report)
    }

    pub(crate) fn ordinary_length(&self) -> Option<u8> {
        self.incumbent_cost?;
        self.shrink.as_ref()?.config.ordinary_length
    }

    pub(crate) fn shrink_stop(&self) -> Option<ShrinkStop> {
        self.shrink.as_ref().and_then(|state| state.stop)
    }

    pub(crate) fn shrink_reserve(
        &mut self,
        workload: &G,
        run: &G::Run,
        campaign_seed: u64,
        reservation: u64,
    ) -> Result<ShrinkReservation<G::Action>, Box<dyn Error>> {
        let Some(state) = self.shrink.as_mut() else {
            return Ok(ShrinkReservation::Unused);
        };
        if state.stop.is_some() {
            return Ok(ShrinkReservation::Stopped);
        }
        let (Some(chain), Some(incumbent)) = (self.chain.as_ref(), self.incumbent_cost) else {
            return Ok(ShrinkReservation::Unused);
        };
        if incumbent <= state.floor {
            state.stop = Some(ShrinkStop::LowerBound);
            return Ok(ShrinkReservation::Stopped);
        }
        let shares = state.shares();
        let generation = state.generation;
        let total = shares.iter().sum::<u64>();
        let mut rand = RomuDuoJrRand::with_seed(campaign_seed ^ reservation ^ KIND_DRAW_SALT);
        let draw = rand.below_u64(NonZeroU64::new(total).ok_or("minimize job shares are empty")?);
        let mut floor = 0;
        let mut drawn = JobKind::Ordinary;
        for kind in JobKind::ALL {
            floor += shares[kind.index()];
            if draw < floor {
                drawn = kind;
                break;
            }
        }
        let record = |kind, shortcut, continuation| ShrinkSelectionRecord {
            kind,
            shares,
            generation,
            shortcut,
            continuation,
        };
        let attempts: &[JobKind] = match drawn {
            JobKind::Continuation => &[JobKind::Continuation, JobKind::Shortcut],
            JobKind::Shortcut => &[JobKind::Shortcut],
            JobKind::Ordinary => &[],
        };
        let candidates = state.config.shortcut_snapshots;
        for attempt in attempts {
            match attempt {
                JobKind::Continuation => {
                    while let Some((entry, index)) = state.queue.pop_front() {
                        let Some(parent_index) = job_origin::<G>(&self.archive, Some(entry)) else {
                            continue;
                        };
                        if index >= chain.actions.len()
                            || !selectable::<G>(
                                &self.archive,
                                self.root,
                                Some(incumbent),
                                parent_index,
                            )
                        {
                            continue;
                        }
                        return Ok(ShrinkReservation::Chain(ChainJob {
                            record: record(
                                JobKind::Continuation,
                                None,
                                Some(ContinuationRecord { entry, index }),
                            ),
                            parent_index,
                            suffix: chain.actions[index..].to_vec(),
                            candidates,
                        }));
                    }
                }
                JobKind::Shortcut => {
                    let primary = state.config.primary_shortcuts;
                    let positions = eligible(chain, self.root, primary);
                    while let Some((start, mut proposal)) =
                        state
                            .schedule
                            .next(workload, run, &chain.actions, &positions, primary)
                    {
                        let shortcut = ShortcutRecord {
                            start,
                            position: state.schedule.position,
                            size: state.schedule.size,
                            variant: state.schedule.variant.saturating_sub(1),
                        };
                        let Some((origin, parent_index)) =
                            (self.root..=start).rev().find_map(|position| {
                                job_origin::<G>(&self.archive, chain.entries[position])
                                    .map(|index| (position, index))
                            })
                        else {
                            continue;
                        };
                        if !selectable::<G>(&self.archive, self.root, Some(incumbent), parent_index)
                        {
                            continue;
                        }
                        let tail = workload.goal_tail(run, proposal.last());
                        proposal.extend(tail);
                        let mut suffix = chain.actions[origin..start].to_vec();
                        suffix.extend(proposal);
                        return Ok(ShrinkReservation::Chain(ChainJob {
                            record: record(JobKind::Shortcut, Some(shortcut), None),
                            parent_index,
                            suffix,
                            candidates,
                        }));
                    }
                }
                JobKind::Ordinary => {}
            }
        }
        if !state.config.ordinary
            || state.config.until_shortcuts_exhausted
                && state.schedule.exhausted
                && state.queue.is_empty()
        {
            state.stop = Some(ShrinkStop::ShortcutsExhausted);
            return Ok(ShrinkReservation::Stopped);
        }
        Ok(ShrinkReservation::Ordinary(record(
            JobKind::Ordinary,
            None,
            None,
        )))
    }

    pub(crate) fn admit_minimize_job(
        &mut self,
        workload: &G,
        run: &G::Run,
        target: Option<&mut G::Target>,
        shrink: Option<&ShrinkSelectionRecord>,
        decisions: &[CampaignAdmissionDecision],
        work: u64,
    ) -> Result<Option<CampaignIncumbentRecord>, Box<dyn Error>> {
        let proposal = match shrink {
            Some(record) if record.kind == JobKind::Ordinary => {
                self.continuation_proposal(decisions)
            }
            _ => None,
        };
        let incumbent = match target {
            Some(target) => self.accept_incumbent(workload, run, target)?,
            None => None,
        };
        let Some(state) = self.shrink.as_mut() else {
            return Ok(incumbent);
        };
        let mut productive = false;
        if let Some(accepted) = &incumbent {
            productive = true;
            let current = shrink.filter(|record| record.generation == state.generation);
            state.generation = state.generation.saturating_add(1);
            state.queue.clear();
            state.proposed.clear();
            if let Some(chain) = self.chain.as_ref() {
                let positions = eligible(chain, self.root, state.config.primary_shortcuts);
                let before = |position: usize| positions.partition_point(|&index| index < position);
                match current.and_then(|record| record.shortcut) {
                    Some(shortcut) => state.schedule.restart_at(before(shortcut.start)),
                    None => state
                        .schedule
                        .restart_at(state.schedule.position.min(before(accepted.rebuilt_from))),
                }
            }
            state
                .accepted
                .add(shrink.map_or(JobKind::Ordinary, |record| record.kind));
        } else if let Some(proposal) = proposal
            && state.queue.len() < CONTINUATION_QUEUE_LIMIT
            && state.proposed.insert(proposal)
        {
            state.queue.push_back(proposal);
            productive = true;
        }
        if let Some(record) = shrink {
            state.jobs.add(record.kind);
            state
                .energy
                .record(record.kind.index(), productive, work.max(1));
        }
        Ok(incumbent)
    }

    fn continuation_proposal(
        &self,
        decisions: &[CampaignAdmissionDecision],
    ) -> Option<(u64, usize)> {
        let chain = self.chain.as_ref()?;
        let incumbent = self.incumbent_cost?;
        let mut best: Option<((u64, u64), u64, usize)> = None;
        for decision in decisions {
            let id = match decision {
                CampaignAdmissionDecision::Retained { id }
                | CampaignAdmissionDecision::Duplicate { id } => *id,
                _ => continue,
            };
            let Some(index) = self.archive.index_of_id(id) else {
                continue;
            };
            let Some(cost) = self.archive.entry_origin_cost(index) else {
                continue;
            };
            if !selectable::<G>(&self.archive, self.root, Some(incumbent), index) {
                continue;
            }
            let key = self.archive.entries[index].key;
            let Some(&chain_index) = chain.slots.get(&slot_of_key(key)) else {
                continue;
            };
            let held = chain.costs[chain_index];
            let saving = held.saving_over(cost);
            if cost < held
                && chain_index < chain.actions.len()
                && best.is_none_or(|(most, _, _)| saving > most)
            {
                best = Some((saving, id, chain_index));
            }
        }
        best.map(|(_, id, chain_index)| (id, chain_index))
    }
}
