// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use super::{
    archive::RetentionPolicy,
    campaign::{CampaignActionResult, CampaignCandidate, CampaignInterfaces, CampaignJobResult},
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Outcome {
    pub dead: bool,
    pub victory: bool,
    pub failed: bool,
}

impl Outcome {
    pub fn terminal(self) -> bool {
        self.dead || self.victory || self.failed
    }
}

pub trait Rollout<G: CampaignInterfaces + ?Sized> {
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

pub struct GameRollout<'a, G: CampaignInterfaces + ?Sized> {
    game: &'a G,
    run: &'a G::Run,
    target: &'a mut G::Target,
}

impl<'a, G: CampaignInterfaces + ?Sized> GameRollout<'a, G> {
    fn new(game: &'a G, run: &'a G::Run, target: &'a mut G::Target) -> Self {
        Self { game, run, target }
    }
}

impl<G: CampaignInterfaces + ?Sized> Rollout<G> for GameRollout<'_, G> {
    fn apply(
        &mut self,
        action: &G::Action,
        milestones: &mut G::Milestones,
    ) -> Result<(), Box<dyn Error>> {
        self.game.apply_action(self.target, action, milestones)
    }

    fn observations(&self) -> Vec<G::Observations> {
        self.game.rollout_observations(self.target)
    }

    fn outcome(&self) -> Result<Outcome, Box<dyn Error>> {
        self.game.rollout_outcome(self.run, self.target)
    }

    fn snapshot(&mut self) -> Result<G::Snapshot, Box<dyn Error>> {
        self.game.snapshot(self.target)
    }

    fn key(&self) -> Result<G::Key, Box<dyn Error>> {
        self.game.rollout_key(self.target)
    }

    fn probe(&mut self, snapshot: &G::Snapshot) -> Result<bool, Box<dyn Error>> {
        self.game.rollout_probe(self.run, self.target, snapshot)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn execute_job<G: CampaignInterfaces + ?Sized>(
    game: &G,
    run: &G::Run,
    target: &mut G::Target,
    origin_snapshot: &G::Snapshot,
    replay: &[G::Action],
    parent_actions: usize,
    parent_milestones: G::Milestones,
    suffix: &[G::Action],
    max_actions: usize,
    retention: RetentionPolicy,
) -> Result<CampaignJobResult<G>, Box<dyn Error>> {
    game.reset(target);
    game.restore(target, origin_snapshot)?;
    let mut replay_milestones = parent_milestones;
    for action in replay {
        game.apply_action(target, action, &mut replay_milestones)?;
        if game.is_terminal(target) {
            break;
        }
    }
    execute_suffix(
        &mut GameRollout::new(game, run, target),
        parent_actions,
        parent_milestones,
        suffix,
        max_actions,
        retention,
    )
}

pub fn execute_suffix<G: CampaignInterfaces + ?Sized>(
    target: &mut impl Rollout<G>,
    parent_actions: usize,
    parent_milestones: G::Milestones,
    suffix: &[G::Action],
    max_actions: usize,
    retention: RetentionPolicy,
) -> Result<CampaignJobResult<G>, Box<dyn Error>> {
    let mut milestones = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    if target.outcome()?.terminal() {
        return Ok(CampaignJobResult { actions });
    }
    for action in suffix {
        if length >= max_actions {
            break;
        }
        length = length.saturating_add(1);
        target.apply(action, &mut milestones)?;
        let observations = target.observations();
        let outcome = target.outcome()?;
        let candidate = if outcome.terminal() {
            None
        } else {
            let snapshot = target.snapshot()?;
            let key = target.key()?;
            let viable = match retention {
                RetentionPolicy::ProbeAtAdmission45 => target.probe(&snapshot)?,
                RetentionPolicy::AdmitAlive => true,
            };
            Some(CampaignCandidate {
                key,
                viable,
                snapshot,
            })
        };
        actions.push(CampaignActionResult {
            action: *action,
            observations,
            milestones,
            dead: outcome.dead,
            victory: outcome.victory,
            failed: outcome.failed,
            candidate,
        });
        if outcome.terminal() {
            break;
        }
    }
    Ok(CampaignJobResult { actions })
}
