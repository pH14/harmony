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
const PRUNE_SLACK: usize = 4_096;

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

pub fn genesis_actions<G: ArcadeGame>(game: &G) -> Result<usize, Box<dyn Error>> {
    let (reset, path) = trail::record_path(|| -> Result<(), Box<dyn Error>> {
        let mut target = game.new_target()?;
        game.reset(&mut target);
        Ok(())
    });
    reset?;
    Ok(path.map_or(0, |path| path.actions.len()))
}

pub fn played<G: ArcadeGame>(
    game: &G,
    root: Option<&G::Snapshot>,
    genesis_actions: usize,
    input: &[ButtonChord],
) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
    let (played, path) = trail::record_path(|| -> Result<(), Box<dyn Error>> {
        let mut target = game.new_target()?;
        match root {
            Some(root) => game.restore(&mut target, root)?,
            None => game.reset(&mut target),
        }
        let mut milestones = G::Milestones::default();
        for action in input {
            game.apply_action(&mut target, action, &mut milestones)?;
            if game.execution_disposition(&target).is_terminal() {
                break;
            }
        }
        Ok(())
    });
    played?;
    let path = path.ok_or("the replay recorded no controller path")?;
    let skip = if path.from_power_on {
        genesis_actions
    } else {
        0
    };
    Ok(path
        .actions
        .get(skip..)
        .ok_or("the replay path is shorter than its starting point")?
        .to_vec())
}

pub fn archived_state<V>(
    id: u64,
    observation: V,
    prefix: &[ButtonChord],
    input: &[ButtonChord],
    frames_at: Option<u64>,
    played: impl FnOnce(&[ButtonChord]) -> Result<Vec<ButtonChord>, Box<dyn Error>>,
) -> Result<State<V>, Box<dyn Error>> {
    let mut chords = prefix.to_vec();
    chords.extend(input);
    if frames_at.is_some_and(|frames| frames != chord_frames(&chords)) {
        chords.truncate(prefix.len());
        chords.extend(played(input)?);
    }
    Ok(State {
        id,
        observation,
        actions: actions(&chords),
        frames: chord_frames(&chords),
    })
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
        stopped: campaign.finished(),
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

    pub fn forget_pending(&mut self) {
        self.pending.clear();
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

    pub fn forget_dropped<G: Workload>(&mut self, campaign: &CampaignCoordinator<'_, G>)
    where
        G::ArchiveReport: Serialize,
    {
        let archive = campaign.archive();
        if self.views.len() <= 2 * archive.live_entry_count() + PRUNE_SLACK {
            return;
        }
        let keep = |id: &u64| *id == 0 || archive.index_of_id(*id).is_some();
        self.views.retain(|id, _| keep(id));
        self.frames_at.retain(|id, _| keep(id));
        self.digests.retain(|id, _| keep(id));
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

#[cfg(test)]
mod tests {
    use machine::nes::ButtonChord;

    use super::{Feed, Note, Stats, Step, archived_state};
    use crate::search::campaign::{CampaignAdmission, CampaignAdmissionDecision};

    fn step(view: u8, digest: Option<u64>, objective: bool) -> Step<u8> {
        Step {
            view,
            frame: u64::from(view) * 10,
            digest,
            objective,
        }
    }

    #[test]
    fn admission_decisions_name_the_retained_steps() {
        let mut feed = Feed::new(0_u8, 0, 99);
        feed.note(Note {
            reservation: 3,
            steps: vec![
                step(1, Some(11), false),
                step(2, None, false),
                step(3, Some(33), true),
                step(4, Some(44), false),
            ],
            trail: Some(vec![0, 1, 2, 3, 4]),
            next_level: 2,
            execution_work: 40,
            death: true,
        });
        feed.note(Note {
            reservation: 4,
            steps: vec![step(5, Some(55), false)],
            trail: None,
            next_level: 0,
            execution_work: 5,
            death: false,
        });
        feed.admit(vec![CampaignAdmission {
            reservation: 3,
            decisions: vec![
                CampaignAdmissionDecision::Retained { id: 7 },
                CampaignAdmissionDecision::Objective,
                CampaignAdmissionDecision::Rejected,
                CampaignAdmissionDecision::Duplicate { id: 7 },
            ],
        }]);
        let batch = feed.take_batch(Stats::default());
        let retained: Vec<_> = batch.points.iter().map(|point| point.retained).collect();
        assert_eq!(retained, [Some(7), None, None, Some(7)]);
        assert_eq!(batch.motion, [vec![0, 1, 2, 3, 4]]);
        assert_eq!((batch.finishes, batch.exits), (vec![0], vec![2]));
        assert_eq!((batch.frames, batch.deaths), (45, 1));
        assert_eq!(feed.view(7), Some(1));
        assert_eq!((feed.frames_at(7), feed.digest(7)), (Some(10), Some(11)));
        assert_eq!(feed.digest(0), Some(99));
        assert!(feed.take_batch(Stats::default()).points.is_empty());
        feed.admit(vec![CampaignAdmission {
            reservation: 4,
            decisions: vec![CampaignAdmissionDecision::Retained { id: 8 }],
        }]);
        assert_eq!(feed.view(8), Some(5));
    }

    #[test]
    fn an_archived_state_replays_only_when_frames_are_missing() {
        let input = [ButtonChord::new(1, 4), ButtonChord::new(2, 6)];
        let prefix = [ButtonChord::new(0, 3)];
        let state = archived_state(5, 'a', &prefix, &input, Some(13), |_| {
            panic!("the input holds every frame")
        })
        .unwrap();
        assert_eq!((state.frames, state.actions.len()), (13, 3));
        let state = archived_state(5, 'a', &prefix, &input, Some(20), |played| {
            assert_eq!(played, input);
            Ok(vec![
                ButtonChord::new(1, 4),
                ButtonChord::new(0, 7),
                ButtonChord::new(2, 6),
            ])
        })
        .unwrap();
        assert_eq!((state.frames, state.actions.len()), (20, 4));
        assert_eq!(state.actions[0].frames, 3);
    }
}
