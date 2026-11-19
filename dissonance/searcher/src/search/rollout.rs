// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};

use super::{
    archive::RetentionPolicy,
    campaign::{CampaignActionResult, CampaignCandidate, CampaignJobResult, Workload},
    choice::{ChoicePlan, Chooser},
    cost::ShrinkCost,
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
#[serde(from = "WireOutcome", into = "WireOutcome")]
pub struct Outcome {
    pub objective_reached: bool,
    pub disposition: ExecutionDisposition,
    pub doomed: bool,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename = "ExecutionDisposition", rename_all = "snake_case")]
enum WireDisposition {
    Runnable,
    Terminal,
    Failed,
    Doomed,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename = "Outcome")]
struct WireOutcome {
    objective_reached: bool,
    disposition: WireDisposition,
}

impl From<Outcome> for WireOutcome {
    fn from(outcome: Outcome) -> Self {
        let disposition = match outcome.disposition {
            ExecutionDisposition::Runnable => WireDisposition::Runnable,
            ExecutionDisposition::Terminal if outcome.doomed => WireDisposition::Doomed,
            ExecutionDisposition::Terminal => WireDisposition::Terminal,
            ExecutionDisposition::Failed => WireDisposition::Failed,
        };
        Self {
            objective_reached: outcome.objective_reached,
            disposition,
        }
    }
}

impl From<WireOutcome> for Outcome {
    fn from(wire: WireOutcome) -> Self {
        let (disposition, doomed) = match wire.disposition {
            WireDisposition::Runnable => (ExecutionDisposition::Runnable, false),
            WireDisposition::Terminal => (ExecutionDisposition::Terminal, false),
            WireDisposition::Failed => (ExecutionDisposition::Failed, false),
            WireDisposition::Doomed => (ExecutionDisposition::Terminal, true),
        };
        Self {
            objective_reached: wire.objective_reached,
            disposition,
            doomed,
        }
    }
}

pub fn classify<G: Workload + ?Sized>(
    workload: &G,
    run: &G::Run,
    target: &G::Target,
) -> Result<Outcome, Box<dyn Error>> {
    let mut outcome = workload.rollout_outcome(run, target)?;
    if outcome.disposition == ExecutionDisposition::Runnable && workload.is_doomed(target) {
        outcome.disposition = ExecutionDisposition::Terminal;
        outcome.doomed = true;
    }
    Ok(outcome)
}

impl Outcome {
    pub fn should_stop(self, stop: &RolloutStop) -> bool {
        self.ends_rollout(stop) || stop.cancelled()
    }

    pub const fn ends_rollout(self, stop: &RolloutStop) -> bool {
        self.disposition.is_terminal() || (stop.objective && self.objective_reached)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct JobCeiling {
    pub start: ShrinkCost,
    pub ceiling: ShrinkCost,
}

#[derive(Clone, Debug)]
pub struct RolloutStop {
    objective: bool,
    cancel: Option<Arc<AtomicBool>>,
    ceiling: Option<JobCeiling>,
    candidates: bool,
    choices: Option<ChoicePlan>,
}

impl RolloutStop {
    pub const fn new(stop_on_objective: bool) -> Self {
        Self {
            objective: stop_on_objective,
            cancel: None,
            ceiling: None,
            candidates: true,
            choices: None,
        }
    }

    #[must_use]
    pub fn with_choices(self, choices: Option<ChoicePlan>) -> Self {
        Self { choices, ..self }
    }

    #[must_use]
    pub const fn choices(&self) -> Option<&ChoicePlan> {
        self.choices.as_ref()
    }

    #[must_use]
    pub fn with_candidates(self, candidates: bool) -> Self {
        Self { candidates, ..self }
    }

    #[must_use]
    pub fn with_cancel(self, cancel: Option<Arc<AtomicBool>>) -> Self {
        Self { cancel, ..self }
    }

    #[must_use]
    pub fn with_ceiling(self, ceiling: Option<JobCeiling>) -> Self {
        Self { ceiling, ..self }
    }

    #[must_use]
    pub const fn ceiling(&self) -> Option<JobCeiling> {
        self.ceiling
    }

    #[must_use]
    pub const fn cancellation(&self) -> Option<&Arc<AtomicBool>> {
        self.cancel.as_ref()
    }

    pub fn cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|cancel| cancel.load(Ordering::Acquire))
    }
}

pub trait Rollout<G: Workload + ?Sized> {
    fn resolve(
        &mut self,
        action: &G::Action,
        chooser: &mut Chooser,
    ) -> Result<G::Action, Box<dyn Error>> {
        let _ = chooser;
        Ok(*action)
    }
    fn apply(
        &mut self,
        action: &G::Action,
        milestones: &mut G::Milestones,
    ) -> Result<(), Box<dyn Error>>;
    fn observations(&self) -> Vec<G::Observations>;
    fn outcome(&self) -> Result<Outcome, Box<dyn Error>>;
    fn shrink_cost(&self, action: &G::Action, observations: &[G::Observations]) -> ShrinkCost;
    fn remaining_cost_bound(&self) -> ShrinkCost;
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
    fn resolve(
        &mut self,
        action: &G::Action,
        chooser: &mut Chooser,
    ) -> Result<G::Action, Box<dyn Error>> {
        self.workload.resolve_action(self.target, action, chooser)
    }

    fn apply(
        &mut self,
        action: &G::Action,
        milestones: &mut G::Milestones,
    ) -> Result<(), Box<dyn Error>> {
        self.workload.verify_integrity()?;
        self.workload
            .apply_action(self.target, action, milestones)?;
        self.workload.verify_integrity()
    }

    fn observations(&self) -> Vec<G::Observations> {
        self.workload.rollout_observations(self.target)
    }

    fn outcome(&self) -> Result<Outcome, Box<dyn Error>> {
        classify(self.workload, self.run, self.target)
    }

    fn shrink_cost(&self, action: &G::Action, observations: &[G::Observations]) -> ShrinkCost {
        self.workload.shrink_cost(action, self.target, observations)
    }

    fn remaining_cost_bound(&self) -> ShrinkCost {
        self.workload.remaining_cost_bound(self.run, self.target)
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
    parent_milestones: G::Milestones,
    suffix: &[G::Action],
    retention: RetentionPolicy,
    stop: RolloutStop,
) -> Result<CampaignJobResult<G>, Box<dyn Error>> {
    workload.verify_integrity()?;
    workload.reset(target);
    workload.restore(target, origin_snapshot)?;
    let mut replay_milestones = parent_milestones;
    if !classify(workload, run, target)?.disposition.is_terminal() {
        for action in replay {
            if stop.cancelled() {
                return Ok(CampaignJobResult {
                    preparation_failure: None,
                    hang: None,
                    actions: Vec::new(),
                });
            }
            workload.apply_action(target, action, &mut replay_milestones)?;
            workload.verify_integrity()?;
            if classify(workload, run, target)?.disposition.is_terminal() {
                break;
            }
        }
    }
    execute_suffix(
        &mut WorkloadRollout::new(workload, run, target),
        parent_milestones,
        suffix,
        retention,
        stop,
    )
}

pub fn execute_suffix<G: Workload + ?Sized>(
    target: &mut impl Rollout<G>,
    parent_milestones: G::Milestones,
    suffix: &[G::Action],
    retention: RetentionPolicy,
    stop: RolloutStop,
) -> Result<CampaignJobResult<G>, Box<dyn Error>> {
    let mut milestones = parent_milestones;
    let mut actions = Vec::with_capacity(suffix.len());
    let parent_outcome = target.outcome()?;
    let mut objective_seen = parent_outcome.objective_reached;
    if parent_outcome.disposition.is_terminal() {
        return Ok(CampaignJobResult {
            preparation_failure: parent_outcome
                .disposition
                .is_failed()
                .then(|| target.observations()),
            hang: None,
            actions,
        });
    }
    let mut running = stop.ceiling.map(|ceiling| ceiling.start);
    let mut chooser = stop.choices.as_ref().map(Chooser::new);
    for drawn in suffix {
        let resolved = match chooser.as_mut() {
            Some(chooser) => target.resolve(drawn, chooser)?,
            None => *drawn,
        };
        let action = &resolved;
        target.apply(action, &mut milestones)?;
        let observations = target.observations();
        let cost = target.shrink_cost(action, &observations);
        let bound = target.remaining_cost_bound();
        let raw_outcome = target.outcome()?;
        let objective_reached = raw_outcome.objective_reached && !objective_seen;
        objective_seen |= raw_outcome.objective_reached;
        let mut outcome = Outcome {
            objective_reached,
            ..raw_outcome
        };
        running = running.map(|running| running + cost);
        if let (Some(running), Some(ceiling)) = (running, stop.ceiling)
            && !objective_reached
            && outcome.disposition == ExecutionDisposition::Runnable
            && running + bound >= ceiling.ceiling
        {
            outcome.disposition = ExecutionDisposition::Terminal;
            outcome.doomed = true;
        }
        let candidate =
            if stop.candidates && matches!(outcome.disposition, ExecutionDisposition::Runnable) {
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
            cost,
            bound,
        });
        if outcome.should_stop(&stop) {
            break;
        }
    }
    Ok(CampaignJobResult {
        preparation_failure: None,
        hang: None,
        actions,
    })
}
