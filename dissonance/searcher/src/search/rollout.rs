// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use serde::{Deserialize, Serialize};

use super::{
    archive::RetentionPolicy,
    campaign::{CampaignActionResult, CampaignCandidate, CampaignJobResult, Workload},
};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionDisposition {
    #[default]
    Runnable,
    Terminal,
    Failed,
}

impl ExecutionDisposition {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal | Self::Failed)
    }

    pub const fn is_failed(self) -> bool {
        matches!(self, Self::Failed)
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Outcome {
    pub objective_reached: bool,
    pub disposition: ExecutionDisposition,
}

impl Outcome {
    pub const fn should_stop(self, stop_rollout_on_objective: bool) -> bool {
        self.disposition.is_terminal() || (stop_rollout_on_objective && self.objective_reached)
    }
}

pub trait Rollout<G: Workload + ?Sized> {
    fn apply(
        &mut self,
        action: &G::Action,
        milestones: &mut G::Milestones,
    ) -> Result<(), Box<dyn Error>>;
    fn observations(&self) -> Vec<G::Observations>;
    fn outcome(&self) -> Result<Outcome, Box<dyn Error>>;
    fn snapshot(&mut self) -> Result<G::Snapshot, Box<dyn Error>>;
    fn key(&self) -> Result<G::Key, Box<dyn Error>>;
    fn probe(&mut self, snapshot: &G::Snapshot) -> Result<bool, Box<dyn Error>>;
}

struct WorkloadRollout<'a, G: Workload + ?Sized> {
    workload: &'a G,
    run: &'a G::Run,
    target: &'a mut G::Target,
}

impl<'a, G: Workload + ?Sized> WorkloadRollout<'a, G> {
    fn new(workload: &'a G, run: &'a G::Run, target: &'a mut G::Target) -> Self {
        Self {
            workload,
            run,
            target,
        }
    }
}

impl<G: Workload + ?Sized> Rollout<G> for WorkloadRollout<'_, G> {
    fn apply(
        &mut self,
        action: &G::Action,
        milestones: &mut G::Milestones,
    ) -> Result<(), Box<dyn Error>> {
        self.workload.apply_action(self.target, action, milestones)
    }

    fn observations(&self) -> Vec<G::Observations> {
        self.workload.rollout_observations(self.target)
    }

    fn outcome(&self) -> Result<Outcome, Box<dyn Error>> {
        self.workload.rollout_outcome(self.run, self.target)
    }

    fn snapshot(&mut self) -> Result<G::Snapshot, Box<dyn Error>> {
        self.workload.snapshot(self.target)
    }

    fn key(&self) -> Result<G::Key, Box<dyn Error>> {
        self.workload.rollout_key(self.target)
    }

    fn probe(&mut self, snapshot: &G::Snapshot) -> Result<bool, Box<dyn Error>> {
        self.workload.rollout_probe(self.run, self.target, snapshot)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn execute_job<G: Workload + ?Sized>(
    workload: &G,
    run: &G::Run,
    target: &mut G::Target,
    origin_snapshot: &G::Snapshot,
    replay: &[G::Action],
    parent_actions: usize,
    parent_milestones: G::Milestones,
    suffix: &[G::Action],
    max_actions: usize,
    retention: RetentionPolicy,
    stop_rollout_on_objective: bool,
) -> Result<CampaignJobResult<G>, Box<dyn Error>> {
    workload.reset(target);
    workload.restore(target, origin_snapshot)?;
    let mut replay_milestones = parent_milestones;
    if !workload
        .rollout_outcome(run, target)?
        .disposition
        .is_terminal()
    {
        for action in replay {
            workload.apply_action(target, action, &mut replay_milestones)?;
            if workload
                .rollout_outcome(run, target)?
                .disposition
                .is_terminal()
            {
                break;
            }
        }
    }
    execute_suffix(
        &mut WorkloadRollout::new(workload, run, target),
        parent_actions,
        parent_milestones,
        suffix,
        max_actions,
        retention,
        stop_rollout_on_objective,
    )
}

pub fn execute_suffix<G: Workload + ?Sized>(
    target: &mut impl Rollout<G>,
    parent_actions: usize,
    parent_milestones: G::Milestones,
    suffix: &[G::Action],
    max_actions: usize,
    retention: RetentionPolicy,
    stop_rollout_on_objective: bool,
) -> Result<CampaignJobResult<G>, Box<dyn Error>> {
    let mut milestones = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    let parent_outcome = target.outcome()?;
    let mut objective_seen = parent_outcome.objective_reached;
    if parent_outcome.disposition.is_terminal() {
        return Ok(CampaignJobResult {
            preparation_failure: parent_outcome
                .disposition
                .is_failed()
                .then(|| target.observations()),
            actions,
        });
    }
    for action in suffix {
        if length >= max_actions {
            break;
        }
        length = length.saturating_add(1);
        target.apply(action, &mut milestones)?;
        let raw_outcome = target.outcome()?;
        let objective_reached = raw_outcome.objective_reached && !objective_seen;
        objective_seen |= raw_outcome.objective_reached;
        let outcome = Outcome {
            objective_reached,
            disposition: raw_outcome.disposition,
        };
        let observations = target.observations();
        let candidate = if matches!(outcome.disposition, ExecutionDisposition::Runnable) {
            let snapshot = target.snapshot()?;
            let key = target.key()?;
            let viable = match retention {
                RetentionPolicy::ProbeAtAdmission => target.probe(&snapshot)?,
                RetentionPolicy::Unprobed => true,
            };
            Some(CampaignCandidate {
                key,
                viable,
                snapshot,
            })
        } else {
            None
        };
        actions.push(CampaignActionResult {
            action: *action,
            observations,
            milestones,
            outcome,
            candidate,
        });
        if outcome.should_stop(stop_rollout_on_objective) {
            break;
        }
    }
    Ok(CampaignJobResult {
        preparation_failure: None,
        actions,
    })
}
