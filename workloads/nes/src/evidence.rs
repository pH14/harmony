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
    pub(crate) aggregate: G::Milestones,
    pub(crate) watermark: G::Progress,
    pub(crate) first_reached: G::MilestoneTimes,
    pub(crate) first_inputs: G::MilestoneInputs,
    pub(crate) champion_input: Input<G::Action>,
    pub(crate) champion_milestones: G::Milestones,
    pub(crate) champion_key: Option<G::ChampionKey>,
    pub(crate) extras: G::Extras,
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
    pub(crate) fn merge_origin(&mut self, watermark: G::Progress) {
        self.watermark = self.watermark.max(watermark);
    }

    pub(crate) fn merge_import(
        &mut self,
        game: &G,
        milestones: G::Milestones,
        input: &Input<G::Action>,
    ) {
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

    pub(crate) fn merge_action<F, P>(
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

    pub(crate) fn checkpoint(&self) -> Result<Vec<u8>, Box<dyn Error>> {
        Ok(postcard::to_allocvec(self)?)
    }

    pub(crate) fn from_checkpoint(bytes: &[u8]) -> Result<Self, Box<dyn Error>> {
        let (evidence, rest) = postcard::take_from_bytes(bytes)?;
        if !rest.is_empty() {
            return Err("search checkpoint evidence has trailing bytes".into());
        }
        Ok(evidence)
    }

    pub(crate) fn checkpoint_marks(&self) -> usize {
        G::first_reached_count(&self.first_reached) + G::named_progress_count(&self.extras)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        num::NonZeroU64,
        path::{Path, PathBuf},
    };

    use serde::Serialize;
    use sha2::{Digest, Sha256};

    use crate::{
        metroid::campaign::{MetroidCampaignRun, MetroidGame},
        mm2::campaign::{Mm2CampaignRun, Mm2Game},
        search::{
            archive::{MAX_ARCHIVE_ENTRIES, RetentionPolicy},
            campaign::{
                CampaignConfig, CampaignExecutionOptions, CampaignOrigin, Workload,
                run_campaign_checkpointed_with_options, search_checkpoint_evidence,
            },
            checkpoint::CheckpointPlan,
            draw::DrawMixture,
        },
        smb::campaign::{SmbCampaignRun, SmbGame, SmbTerminalPredicate},
        stb::{
            campaign::{StbCampaignRun, StbGame},
            target::StbAi,
        },
    };

    fn checkpoints(directory: &Path, reason: &str) -> Vec<PathBuf> {
        let suffix = format!("-{reason}.ckpt");
        let mut found = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_string_lossy().ends_with(&suffix))
            .collect::<Vec<_>>();
        found.sort();
        found
    }

    fn assert_resume_restores_evidence<G>(game: &G, run: G::Run, label: &str)
    where
        G: Workload,
        G::ArchiveReport: Serialize,
    {
        let directory = std::env::temp_dir().join(format!(
            "nes-evidence-resume-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&directory);
        let config = CampaignConfig {
            campaign_seed: 7,
            workers: 2,
            execution_budget: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            stop_rollout_on_objective: true,
            stop_campaign_on_objective: false,
            archive_entry_limit: MAX_ARCHIVE_ENTRIES,
            window: 4,
            memory_budget_mib: Some(512),
            materialize_final_artifacts: false,
            run,
            mixture: DrawMixture::EnergySplice { scale: 6 },
            retention: RetentionPolicy::Unprobed,
            objective_witness_path: None,
        };
        let search = |origin: CampaignOrigin<G>, directory: &Path, every| {
            run_campaign_checkpointed_with_options(
                game,
                &config,
                &origin,
                &mut std::io::sink(),
                None,
                CampaignExecutionOptions {
                    checkpoints: Some(CheckpointPlan {
                        directory: directory.to_path_buf(),
                        every,
                        on_marks: true,
                        on_top_progress: true,
                    }),
                    ..CampaignExecutionOptions::default()
                },
            )
            .unwrap_or_else(|error| panic!("{label}: {error}"))
        };
        let final_evidence = |directory: &Path| {
            let [path] = <[PathBuf; 1]>::try_from(checkpoints(directory, "final")).unwrap();
            search_checkpoint_evidence::<G>(&path).unwrap()
        };

        let original_directory = directory.join("original");
        let (original, _) = search(
            CampaignOrigin::Genesis,
            &original_directory,
            NonZeroU64::new(48),
        );
        let interval = checkpoints(&original_directory, "interval")
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("{label} wrote no interval checkpoint"));
        let stored = search_checkpoint_evidence::<G>(&interval).unwrap();
        let restored = G::evidence_from_checkpoint(&stored).unwrap();
        assert_eq!(
            G::evidence_checkpoint(&restored).unwrap(),
            stored,
            "{label}"
        );
        assert!(G::evidence_from_checkpoint(&stored[..stored.len() - 1]).is_err());
        let mut padded = stored.clone();
        padded.push(0);
        assert!(G::evidence_from_checkpoint(&padded).is_err());

        let resumed_directory = directory.join("resumed");
        let (resumed, _) = search(
            CampaignOrigin::SearchCheckpoint { path: interval },
            &resumed_directory,
            None,
        );
        assert_eq!(
            final_evidence(&resumed_directory),
            final_evidence(&original_directory),
            "{label}"
        );
        assert_eq!(
            serde_json::to_value(&resumed.archive).unwrap(),
            serde_json::to_value(&original.archive).unwrap(),
            "{label}"
        );
        assert_eq!(resumed.executions_completed, original.executions_completed);
        fs::remove_dir_all(&directory).unwrap();
    }

    fn rom(variable: &str) -> Option<Vec<u8>> {
        std::env::var_os(variable).map(|path| fs::read(path).unwrap())
    }

    #[test]
    fn every_nes_game_resumes_a_search_checkpoint_with_its_evidence() {
        let smb_run = SmbCampaignRun {
            vocabulary: Default::default(),
            terminal: Some(SmbTerminalPredicate::GameVictory),
        };
        assert_resume_restores_evidence(
            &SmbGame::loopback_for_tests(&[0]),
            smb_run,
            "smb-loopback",
        );
        let roms = [
            "HARMONY_SMB_ROM",
            "HARMONY_MM2_ROM",
            "HARMONY_METROID_ROM",
            "HARMONY_STB_ROM",
        ]
        .map(rom);
        if roms.iter().all(Option::is_none) {
            return;
        }
        let core = PathBuf::from(
            std::env::var_os("HARMONY_QUICKNES_CORE")
                .expect("HARMONY_QUICKNES_CORE must name QuickNES when a game ROM is set"),
        );
        let core_sha256 = format!("{:x}", Sha256::digest(fs::read(&core).unwrap()));
        let [smb, mm2, metroid, stb] = roms;
        if let Some(rom) = smb {
            let game = SmbGame::new(&rom, &core, &core_sha256);
            assert_resume_restores_evidence(&game, smb_run, "smb");
        }
        if let Some(rom) = mm2 {
            let game = Mm2Game::new_whole_game(&rom, &core, &core_sha256, Vec::new());
            assert_resume_restores_evidence(&game, Mm2CampaignRun, "mm2");
        }
        if let Some(rom) = metroid {
            let game = MetroidGame::new(&rom, &core, &core_sha256);
            assert_resume_restores_evidence(&game, MetroidCampaignRun, "metroid");
        }
        if let Some(rom) = stb {
            let game = StbGame::with_ai(&rom, &core, &core_sha256, StbAi::Hard);
            assert_resume_restores_evidence(&game, StbCampaignRun, "stb");
        }
    }
}
