// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, error::Error, fmt::Debug};

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::{
    catalog::Record,
    checks::{Checker, Finding},
    compile::{Accumulator, Compiled},
    key::AssertionKey,
    records::RecordedAction,
};
use crate::search::{
    archive::{Archive, ArchiveCandidate},
    campaign::CampaignAdmissionDecision,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecordedJob {
    pub sequence: u64,
    pub parent_id: u64,
    pub decisions: Vec<CampaignAdmissionDecision>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Tier {
    pub progress: u32,
    pub cells: usize,
    pub entries: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Holders {
    pub progress: u32,
    pub place: u64,
    pub identity: u32,
    pub ids: Vec<u64>,
    pub keys: Vec<AssertionKey>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Rekeyed {
    pub policy: String,
    pub jobs: usize,
    pub candidates: usize,
    pub retained: u64,
    pub rejected: u64,
    pub cells: usize,
    pub tiers: Vec<Tier>,
    pub holders: Vec<Holders>,
    pub snapshots: Vec<u64>,
    pub findings: Vec<Finding>,
    pub decisions: Vec<Vec<CampaignAdmissionDecision>>,
}

#[derive(Clone)]
struct Node {
    anchor: usize,
    suffix: Vec<usize>,
    recorded: Accumulator,
    keyed: Accumulator,
    stopped: bool,
}

enum Step {
    Candidate(AssertionKey),
    Stopped,
    Continue,
}

fn fold(
    strategy: &Compiled,
    frames: &[Record],
    accumulator: &mut Accumulator,
    mut checker: Option<&mut Checker<'_>>,
) -> Step {
    let mut key = None;
    for (index, frame) in frames.iter().enumerate() {
        let parent = checker.is_some().then(|| accumulator.clone());
        let observed = strategy.observe(frame, accumulator);
        let stopped = observed.goal || observed.rejected.is_some();
        if (stopped || index + 1 == frames.len())
            && let (Some(checker), Some(parent)) = (checker.as_deref_mut(), &parent)
        {
            checker.observe(frame, parent);
        }
        if stopped {
            return Step::Stopped;
        }
        key = observed.key;
    }
    key.map_or(Step::Continue, Step::Candidate)
}

pub struct Rekeying<'a> {
    pub recorded: &'a Compiled,
    pub strategy: &'a Compiled,
    pub entry_limit: usize,
}

impl Rekeying<'_> {
    pub fn run<A>(
        &self,
        genesis: &Record,
        jobs: &[RecordedJob],
        actions: &[RecordedAction<A>],
        action_cost: fn(&A) -> u64,
    ) -> Result<Rekeyed, Box<dyn Error>>
    where
        A: Clone + Debug + Eq + Ord + Serialize + DeserializeOwned,
    {
        let mut archive: Archive<A, AssertionKey, (), ()> =
            Archive::with_shape(action_cost, self.strategy.shape());
        archive.max_entries = self.entry_limit;
        let mut recorded = Accumulator::default();
        let _ = self.recorded.observe(genesis, &mut recorded);
        let mut keyed = Accumulator::default();
        let keyed_genesis = self.strategy.observe(genesis, &mut keyed);
        let genesis_key = keyed_genesis
            .key
            .ok_or("the genesis record has no key under the strategy")?;
        let root = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: Vec::new(),
                    key: genesis_key,
                    milestones: (),
                },
                (),
            )?
            .ok_or("the genesis entry was not retained")?;
        let mut nodes: BTreeMap<u64, Node> = BTreeMap::new();
        nodes.insert(
            0,
            Node {
                anchor: root,
                suffix: Vec::new(),
                recorded,
                keyed,
                stopped: false,
            },
        );
        let mut by_sequence: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
        for (index, action) in actions.iter().enumerate() {
            by_sequence.entry(action.sequence).or_default().push(index);
        }
        let mut checker = self.strategy.checker();
        let mut candidates = 0;
        let mut decisions = Vec::with_capacity(jobs.len());
        for job in jobs {
            let parent = nodes.get(&job.parent_id).ok_or_else(|| {
                format!(
                    "job {} starts from entry {} that the recording never retained",
                    job.sequence, job.parent_id
                )
            })?;
            let mut place = parent.clone();
            let mut pending = std::mem::take(&mut place.suffix);
            let mut previous_key = None;
            let mut keyed_stopped = place.stopped;
            let mut recorded_decisions = job
                .decisions
                .iter()
                .filter(|decision| !matches!(decision, CampaignAdmissionDecision::Objective));
            let mut made = Vec::new();
            for &index in by_sequence
                .get(&job.sequence)
                .map_or(&[][..], Vec::as_slice)
            {
                let action = &actions[index];
                pending.push(index);
                let recorded_step = if action.failed {
                    Step::Stopped
                } else {
                    fold(self.recorded, &action.frames, &mut place.recorded, None)
                };
                let keyed_step = if action.failed || keyed_stopped {
                    Step::Stopped
                } else {
                    fold(
                        self.strategy,
                        &action.frames,
                        &mut place.keyed,
                        Some(&mut checker),
                    )
                };
                if let Step::Candidate(key) = keyed_step {
                    candidates += 1;
                    let suffix: Vec<A> = pending
                        .iter()
                        .map(|index| actions[*index].action.clone())
                        .collect();
                    let certificates = vec![None; suffix.len()];
                    let retained_before = archive.retained;
                    let (admitted, completed) = archive.insert_after_certified(
                        Some(place.anchor),
                        previous_key,
                        job.sequence,
                        ArchiveCandidate {
                            suffix: suffix.as_slice(),
                            key,
                            milestones: (),
                        },
                        (),
                        Some(&certificates),
                    )?;
                    match admitted {
                        Some(id) => {
                            let stable =
                                archive.stable_id(id).ok_or("admitted entry is missing")?;
                            made.push(if archive.retained > retained_before {
                                CampaignAdmissionDecision::Retained { id: stable }
                            } else {
                                CampaignAdmissionDecision::Duplicate { id: stable }
                            });
                            place.anchor = id;
                            pending.clear();
                            previous_key = None;
                        }
                        None => {
                            made.push(CampaignAdmissionDecision::Rejected);
                            previous_key = Some(completed);
                        }
                    }
                } else if matches!(keyed_step, Step::Stopped) {
                    keyed_stopped = true;
                }
                if let Step::Candidate(_) = recorded_step {
                    match recorded_decisions.next() {
                        Some(
                            CampaignAdmissionDecision::Retained { id }
                            | CampaignAdmissionDecision::Duplicate { id },
                        ) => {
                            nodes.entry(*id).or_insert_with(|| Node {
                                anchor: place.anchor,
                                suffix: pending.clone(),
                                recorded: place.recorded.clone(),
                                keyed: place.keyed.clone(),
                                stopped: keyed_stopped,
                            });
                        }
                        Some(_) => {}
                        None => {
                            return Err(format!(
                                "job {} has more candidates in its records than decisions in the stream",
                                job.sequence
                            )
                            .into());
                        }
                    }
                }
            }
            if recorded_decisions.next().is_some() {
                return Err(format!(
                    "job {} has more decisions in the stream than candidates in its records",
                    job.sequence
                )
                .into());
            }
            decisions.push(made);
        }
        let mut tiers: BTreeMap<u32, (usize, usize)> = BTreeMap::new();
        let mut cells = std::collections::BTreeSet::new();
        let mut holders = Vec::new();
        for ((cell, identity), held) in &archive.slots {
            let ids: Vec<usize> = held
                .iter()
                .copied()
                .filter(|id| archive.active[*id])
                .collect();
            if ids.is_empty() {
                continue;
            }
            cells.insert(*cell);
            let tier = tiers.entry(cell.0).or_default();
            tier.1 += ids.len();
            holders.push(Holders {
                progress: cell.0,
                place: cell.1.get(),
                identity: *identity,
                ids: ids
                    .iter()
                    .map(|id| archive.stable_id(*id).ok_or("holder is missing"))
                    .collect::<Result<_, _>>()?,
                keys: ids
                    .iter()
                    .map(|id| archive.entry_key(*id).ok_or("holder key is missing"))
                    .collect::<Result<_, _>>()?,
            });
        }
        for cell in &cells {
            tiers.entry(cell.0).or_default().0 += 1;
        }
        let snapshots = archive
            .take_entry_reports_and_snapshots()
            .1
            .into_iter()
            .map(|(id, ())| id)
            .collect();
        Ok(Rekeyed {
            policy: self.strategy.policy_identifier().to_owned(),
            jobs: jobs.len(),
            candidates,
            retained: archive.retained,
            rejected: archive.rejected,
            cells: cells.len(),
            tiers: tiers
                .into_iter()
                .rev()
                .map(|(progress, (cells, entries))| Tier {
                    progress,
                    cells,
                    entries,
                })
                .collect(),
            holders,
            snapshots,
            findings: checker.findings(),
            decisions,
        })
    }
}

#[must_use]
pub fn same_decisions(jobs: &[RecordedJob], rekeyed: &Rekeyed) -> usize {
    jobs.iter()
        .zip(&rekeyed.decisions)
        .filter(|(job, made)| {
            job.decisions
                .iter()
                .filter(|decision| !matches!(decision, CampaignAdmissionDecision::Objective))
                .eq(made.iter())
        })
        .count()
}
