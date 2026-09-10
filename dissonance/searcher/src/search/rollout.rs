// SPDX-License-Identifier: AGPL-3.0-or-later

//! Action-boundary rollout mechanics shared by workload adapters.

use std::error::Error;

use super::{
    archive::RetentionPolicy,
    campaign::{CampaignActionResult, CampaignCandidate, CampaignInterfaces, CampaignJobResult},
};

/// Workload classification at an action boundary.
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

/// Workload operations required by the engine's suffix loop.
///
/// A probe restores the supplied snapshot, including adapter observation state,
/// before returning. Its execution cost may advance lifetime accounting.
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

/// Adapter from the game contract to the action-boundary rollout engine.
/// Workloads provide target-specific observations, outcome classification, and
/// probes through the optional `Game::rollout_*` seams; restore, snapshot, key,
/// and action application remain owned by the generic game contract.
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

/// Execute one worker job from an origin snapshot through its parent path and
/// suffix. The target is reset before restoring the origin so worker reuse
/// cannot carry adapter state across jobs. Replay milestones are intentionally
/// local: the parent archive already owns the aggregate passed to the suffix.
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

/// Extend a restored parent, preserving action evidence before probing candidates.
///
/// Workloads own action meaning and evaluation. The engine owns limits,
/// candidate capture, probe placement, result assembly, and terminal stopping.
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
            discard_previous_dead: false,
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

/// One opt-in retry from the preceding live state, within an already bounded
/// sequence of ordinary draws. It draws no extra actions and owns one temporary
/// snapshot. Every attempted action must remain in the worker result and cost.
/// Only `dead && !failed && !victory` can request a restore. A second consecutive
/// dead attempt stops; a live step starts a new opportunity.
pub struct LocalRetry<S, M> {
    live: Option<(S, M)>,
    pending: bool,
    retrying: bool,
}
impl<S: Clone, M: Copy> LocalRetry<S, M> {
    pub fn new(live: Option<(S, M)>) -> Self {
        Self {
            live,
            pending: false,
            retrying: false,
        }
    }

    /// Restore only when the preceding recorded attempt requested a retry.
    /// The returned milestones replace the abandoned branch's accumulator.
    pub fn restore_pending(
        &mut self,
        restore: impl FnOnce(&S) -> Result<(), Box<dyn Error>>,
    ) -> Result<Option<M>, Box<dyn Error>> {
        if !self.pending {
            return Ok(None);
        }
        let (snapshot, milestones) = self.live.as_ref().ok_or("retry lacks a live snapshot")?;
        restore(snapshot)?;
        self.pending = false;
        self.retrying = true;
        Ok(Some(*milestones))
    }

    /// Return whether another pre-drawn action may execute. The caller still
    /// enforces its existing attempt, input and physical-work limits.
    pub fn observe(
        &mut self,
        outcome: Outcome,
        snapshot: Option<&S>,
        milestones: M,
    ) -> Result<bool, Box<dyn Error>> {
        if outcome.failed || outcome.victory {
            return Ok(false);
        }
        if outcome.dead {
            self.pending = self.live.is_some() && !self.retrying;
            return Ok(self.pending);
        }
        if self.live.is_some() {
            self.live = Some((
                snapshot
                    .ok_or("live retry boundary lacks a snapshot")?
                    .clone(),
                milestones,
            ));
        }
        self.retrying = false;
        Ok(true)
    }
}

#[cfg(test)]
mod local_retry_tests {
    use super::*;
    #[test]
    fn restores_prior_milestones_and_propagates_restore_failures() {
        let mut retry = LocalRetry::new(Some((7_u8, 10_u8)));
        assert!(
            retry
                .observe(
                    Outcome {
                        dead: true,
                        ..Outcome::default()
                    },
                    None,
                    99
                )
                .unwrap()
        );
        assert!(
            retry
                .restore_pending(|_| Err("restore failed".into()))
                .is_err()
        );
        let mut restored = 0;
        assert_eq!(
            retry
                .restore_pending(|snapshot| {
                    restored = *snapshot;
                    Ok(())
                })
                .unwrap(),
            Some(10)
        );
        assert_eq!(restored, 7);
        assert!(
            !retry
                .observe(
                    Outcome {
                        dead: true,
                        ..Outcome::default()
                    },
                    None,
                    88
                )
                .unwrap()
        );
    }
}
