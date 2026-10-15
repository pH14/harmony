// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, error::Error};

use machine::{
    MachineError,
    nes::{ButtonChord, core_state},
};
use serde::{Deserialize, Serialize};

use super::{ArcadeGame, snapshot_digest, trail};
use crate::{
    nes_backend::unpack_state_into,
    search::{
        campaign::{
            CampaignAdmission, CampaignAdmissionDecision, CampaignCoordinator, CampaignJobResult,
            JobOutcome, JobSpec, WatchedJob, Workload,
        },
        rollout::ExecutionDisposition,
    },
};

const MIN_TRAIL_SAMPLES: usize = 6;

#[derive(Clone, Deserialize, Serialize)]
#[serde(bound = "V: Serialize + serde::de::DeserializeOwned")]
pub struct Step<V> {
    pub view: V,
    pub frame: u64,
    pub digest: Option<u64>,
    pub objective: bool,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(bound = "V: Serialize + serde::de::DeserializeOwned")]
pub struct Note<V> {
    pub reservation: usize,
    pub steps: Vec<Step<V>>,
    pub trail: Option<Vec<u16>>,
    pub next_level: u8,
    pub execution_work: u64,
    pub death: bool,
}

#[derive(Clone, Copy, Serialize)]
pub struct Point<V> {
    pub observation: V,
    pub retained: Option<u64>,
    pub frame: u64,
}

#[derive(Clone, Copy, Default, Serialize)]
pub struct Stats {
    pub executions: u64,
    pub states: usize,
    pub snapshot_bytes: usize,
    pub stopped: bool,
    pub won: bool,
}

#[derive(Serialize)]
pub struct Batch<V> {
    #[serde(flatten)]
    pub stats: Stats,
    pub deaths: u64,
    pub frames: u64,
    pub points: Vec<Point<V>>,
    pub motion: Vec<Vec<u16>>,
    pub finishes: Vec<usize>,
    pub exits: Vec<u8>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct Action {
    pub buttons: u8,
    pub frames: u8,
}

#[derive(Serialize)]
pub struct State<V> {
    pub id: u64,
    pub observation: V,
    pub actions: Vec<Action>,
    pub frames: u64,
}

#[must_use]
pub fn actions(chords: &[ButtonChord]) -> Vec<Action> {
    chords
        .iter()
        .map(|chord| Action {
            buttons: chord.buttons,
            frames: chord.bounded_hold_frames(),
        })
        .collect()
}

#[must_use]
pub fn chord_frames(chords: &[ButtonChord]) -> u64 {
    chords
        .iter()
        .map(|chord| u64::from(chord.bounded_hold_frames()))
        .sum()
}

pub fn raw_state(packed: &[u8]) -> Result<Vec<u8>, MachineError> {
    let declared = packed
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| MachineError::Backend("snapshot has no size prefix".to_owned()))?;
    let mut state = Vec::new();
    unpack_state_into(packed, declared as usize, &mut state)?;
    Ok(state)
}

pub fn steps<G: ArcadeGame>(
    result: &CampaignJobResult<G>,
) -> Result<Vec<Step<G::View>>, MachineError> {
    let mut steps = Vec::with_capacity(result.actions.len());
    for action in &result.actions {
        let Some(last) = action.observations.last() else {
            continue;
        };
        let digest = match &action.candidate {
            Some(candidate) => Some(snapshot_digest(core_state(&raw_state(G::emulator_state(
                &candidate.snapshot,
            ))?))),
            None => None,
        };
        steps.push(Step {
            view: G::view(last),
            frame: G::frame_count(last),
            digest,
            objective: action.outcome.objective_reached,
        });
    }
    Ok(steps)
}

pub fn run_noted<G: ArcadeGame>(
    job: &JobSpec<G>,
    run: impl FnOnce() -> Result<JobOutcome<G>, Box<dyn Error>>,
) -> WatchedJob<G, Note<G::View>> {
    let (outcome, trail) = trail::record(chord_frames(&job.replay), run);
    let outcome = outcome?;
    let next_level = outcome
        .result
        .actions
        .iter()
        .flat_map(|action| action.observations.last())
        .map(G::cleared)
        .find(|&cleared| cleared > trail.start_cleared)
        .unwrap_or(0);
    let death = outcome.result.actions.last().is_some_and(|action| {
        action.outcome.disposition == ExecutionDisposition::Terminal
            && !action.outcome.objective_reached
    });
    let note = Note {
        reservation: outcome.reservation,
        steps: steps(&outcome.result)?,
        trail: (trail.samples.len() >= MIN_TRAIL_SAMPLES).then_some(trail.samples),
        next_level,
        execution_work: outcome.execution_work,
        death,
    };
    Ok((outcome, note))
}

#[must_use]
pub fn stats<G: Workload>(campaign: &CampaignCoordinator<'_, G>) -> Stats
where
    G::ArchiveReport: Serialize,
{
    let won = campaign.objective_witness().is_some();
    Stats {
        executions: campaign.executions(),
        states: campaign.archive().active_count(),
        snapshot_bytes: campaign.archive().resident_snapshot_bytes(),
        stopped: won || campaign.finished(),
        won,
    }
}

pub struct Feed<V> {
    pending: BTreeMap<usize, Note<V>>,
    views: BTreeMap<u64, V>,
    frames_at: BTreeMap<u64, u64>,
    digests: BTreeMap<u64, u64>,
    points: Vec<Point<V>>,
    motion: Vec<Vec<u16>>,
    finishes: Vec<usize>,
    exits: Vec<u8>,
    frames: u64,
    deaths: u64,
}

impl<V: Copy> Feed<V> {
    pub fn new(root: V, root_frames: u64, root_digest: u64) -> Self {
        Self {
            pending: BTreeMap::new(),
            views: BTreeMap::from([(0, root)]),
            frames_at: BTreeMap::from([(0, root_frames)]),
            digests: BTreeMap::from([(0, root_digest)]),
            points: Vec::new(),
            motion: Vec::new(),
            finishes: Vec::new(),
            exits: Vec::new(),
            frames: 0,
            deaths: 0,
        }
    }

    pub fn note(&mut self, note: Note<V>) {
        self.frames = self.frames.saturating_add(note.execution_work);
        self.deaths += u64::from(note.death);
        self.pending.insert(note.reservation, note);
    }

    pub fn admit(&mut self, admissions: Vec<CampaignAdmission>) {
        for admission in admissions {
            let Some(note) = self.pending.remove(&admission.reservation) else {
                continue;
            };
            let mut decisions = admission.decisions.into_iter();
            for step in note.steps {
                if step.objective {
                    decisions.next();
                }
                let retained = if step.digest.is_some() {
                    match decisions.next() {
                        Some(
                            CampaignAdmissionDecision::Retained { id }
                            | CampaignAdmissionDecision::Duplicate { id },
                        ) => Some(id),
                        _ => None,
                    }
                } else {
                    None
                };
                if let (Some(id), Some(digest)) = (retained, step.digest)
                    && !self.views.contains_key(&id)
                {
                    self.views.insert(id, step.view);
                    self.frames_at.insert(id, step.frame);
                    self.digests.insert(id, digest);
                }
                self.points.push(Point {
                    observation: step.view,
                    retained,
                    frame: step.frame,
                });
            }
            if let Some(trail) = note.trail {
                if note.next_level > 0 {
                    self.finishes.push(self.motion.len());
                    self.exits.push(note.next_level);
                }
                self.motion.push(trail);
            }
        }
    }

    pub fn take_batch(&mut self, stats: Stats) -> Batch<V> {
        Batch {
            stats,
            deaths: self.deaths,
            frames: self.frames,
            points: std::mem::take(&mut self.points),
            motion: std::mem::take(&mut self.motion),
            finishes: std::mem::take(&mut self.finishes),
            exits: std::mem::take(&mut self.exits),
        }
    }

    #[must_use]
    pub fn view(&self, id: u64) -> Option<V> {
        self.views.get(&id).copied()
    }

    #[must_use]
    pub fn frames_at(&self, id: u64) -> Option<u64> {
        self.frames_at.get(&id).copied()
    }

    #[must_use]
    pub fn digest(&self, id: u64) -> Option<u64> {
        self.digests.get(&id).copied()
    }
}
