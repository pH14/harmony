// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::search::{
    archive::Input,
    campaign::{CampaignActionResult, Evaluation},
};

pub trait NesEvidence: Evaluation<Progress: Ord> {
    type MilestoneTimes: Copy + Default + Eq + Serialize + DeserializeOwned;
    type MilestoneInputs: Clone + Default + Serialize + DeserializeOwned;
    type ChampionKey: Copy + Ord + Serialize + DeserializeOwned;
    type Extras: Clone + Default + Serialize + DeserializeOwned;

    fn milestone_rank(milestones: Self::Milestones) -> impl Ord;
    fn merge_watermark(watermark: &mut Self::Progress, observations: &[Self::Observations]);
    fn champion_key(
        observations: &[Self::Observations],
        milestones: Self::Milestones,
    ) -> Option<Self::ChampionKey>;
    fn imported_champion_key(_milestones: Self::Milestones) -> Option<Self::ChampionKey> {
        None
    }
    fn record_first_reached(
        times: &mut Self::MilestoneTimes,
        inputs: &mut Self::MilestoneInputs,
        extras: &Self::Extras,
        milestones: Self::Milestones,
        sequence: u64,
        input: &Input<Self::Action>,
    );
    fn first_reached_count(times: &Self::MilestoneTimes) -> usize;
    fn named_progress_count(_extras: &Self::Extras) -> usize {
        0
    }
}

#[derive(Deserialize, Serialize)]
#[serde(bound = "")]
pub struct NesCampaignEvidence<G: NesEvidence> {
    pub aggregate: G::Milestones,
    pub watermark: G::Progress,
    pub first_reached: G::MilestoneTimes,
    pub first_inputs: G::MilestoneInputs,
    pub champion_input: Input<G::Action>,
    pub champion_milestones: G::Milestones,
    pub champion_key: Option<G::ChampionKey>,
    pub extras: G::Extras,
}

impl<G: NesEvidence> Clone for NesCampaignEvidence<G> {
    fn clone(&self) -> Self {
        Self {
            aggregate: self.aggregate,
            watermark: self.watermark,
            first_reached: self.first_reached,
            first_inputs: self.first_inputs.clone(),
            champion_input: self.champion_input.clone(),
            champion_milestones: self.champion_milestones,
            champion_key: self.champion_key,
            extras: self.extras.clone(),
        }
    }
}

impl<G: NesEvidence> Default for NesCampaignEvidence<G> {
    fn default() -> Self {
        Self {
            aggregate: G::Milestones::default(),
            watermark: G::Progress::default(),
            first_reached: G::MilestoneTimes::default(),
            first_inputs: G::MilestoneInputs::default(),
            champion_input: Input::default(),
            champion_milestones: G::Milestones::default(),
            champion_key: None,
            extras: G::Extras::default(),
        }
    }
}

impl<G: NesEvidence> NesCampaignEvidence<G> {
    pub fn merge_origin(&mut self, watermark: G::Progress) {
        self.watermark = self.watermark.max(watermark);
    }

    pub fn merge_import(&mut self, game: &G, milestones: G::Milestones, input: &Input<G::Action>) {
        game.merge_milestones(&mut self.aggregate, milestones);
        G::record_first_reached(
            &mut self.first_reached,
            &mut self.first_inputs,
            &self.extras,
            milestones,
            0,
            input,
        );
        if G::milestone_rank(milestones) > G::milestone_rank(self.champion_milestones) {
            self.champion_milestones = milestones;
            self.champion_input = input.clone();
            if let Some(key) = G::imported_champion_key(milestones) {
                self.champion_key = Some(key);
            }
        }
    }

    pub fn merge_action<F, P>(
        &mut self,
        game: &G,
        action: &CampaignActionResult<G>,
        sequence: u64,
        extras_need_input: bool,
        input: F,
        publish: P,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<G::Action>, Box<dyn Error>>,
        P: FnOnce(&Input<G::Action>, bool) -> Result<(), Box<dyn Error>>,
    {
        G::merge_watermark(&mut self.watermark, &action.observations);
        game.merge_milestones(&mut self.aggregate, action.milestones);
        let first_reached = self.reaches_a_new_milestone(action.milestones);
        let champion = G::champion_key(&action.observations, action.milestones)
            .filter(|key| self.champion_key.is_none_or(|current| *key > current));
        if !first_reached && champion.is_none() && !extras_need_input {
            return Ok(());
        }
        let input = input()?;
        publish(&input, champion.is_some())?;
        G::record_first_reached(
            &mut self.first_reached,
            &mut self.first_inputs,
            &self.extras,
            action.milestones,
            sequence,
            &input,
        );
        if let Some(key) = champion {
            self.champion_key = Some(key);
            self.champion_milestones = action.milestones;
            self.champion_input = input;
        }
        Ok(())
    }

    fn reaches_a_new_milestone(&self, milestones: G::Milestones) -> bool {
        let mut times = self.first_reached;
        G::record_first_reached(
            &mut times,
            &mut G::MilestoneInputs::default(),
            &self.extras,
            milestones,
            0,
            &Input::default(),
        );
        times != self.first_reached
    }

    pub fn checkpoint(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        Ok(postcard::to_allocvec(self)?)
    }

    pub fn from_checkpoint(bytes: &[u8]) -> Result<Self, Box<dyn Error>> {
        let (evidence, rest) = postcard::take_from_bytes(bytes)?;
        if !rest.is_empty() {
            return Err("search checkpoint evidence has trailing bytes".into());
        }
        Ok(evidence)
    }

    pub fn checkpoint_marks(&self) -> usize {
        G::first_reached_count(&self.first_reached) + G::named_progress_count(&self.extras)
    }
}
