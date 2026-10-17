// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write, path::PathBuf};

use machine::quicknes::QuickNesMachine;

pub use nes_games::twin_dragons::campaign::{
    CAMPAIGN_STREAM_FORMAT, SNAPSHOT_CHECKPOINT_FORMAT, TwinDragonsCampaignCheckpoint,
    TwinDragonsCampaignEvidence, TwinDragonsCampaignModeReport, TwinDragonsCampaignRun,
    TwinDragonsCampaignStreamHeader, TwinDragonsSnapshotCheckpoint, TwinDragonsTargets,
};

use crate::{
    search::{
        archive::RetentionPolicy,
        campaign::{
            ArchiveSource, CampaignConfig, CampaignOrigin, TargetExecution,
            replay_campaign_checkpointed, run_campaign_checkpointed,
        },
        draw::DrawMixture,
    },
    target::{ExitKind, Target},
    twin_dragons::{archive::TwinDragonsArchiveReport, target::TwinDragonsInput},
};

pub type TwinDragonsGame = nes_games::twin_dragons::campaign::TwinDragonsGame<QuickNesMachine>;
pub type TwinDragonsCampaignOrigin = CampaignOrigin<TwinDragonsGame>;

pub struct TwinDragonsCampaignConfig {
    pub campaign_seed: u64,
    pub workers: u32,
    pub execution_budget: u64,
    pub host: String,
    pub wall_budget: Option<std::time::Duration>,
    pub continue_after_victory: bool,
    pub archive_entry_limit: usize,
    pub memory_budget_mib: Option<usize>,
    pub materialize_final_artifacts: bool,
    pub retention: RetentionPolicy,
    pub mixture: DrawMixture,
    pub victory_input_path: Option<PathBuf>,
}

impl TwinDragonsCampaignConfig {
    fn generic(&self) -> CampaignConfig<TwinDragonsGame> {
        CampaignConfig {
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
            stop_rollout_on_objective: !self.continue_after_victory,
            stop_campaign_on_objective: !self.continue_after_victory,
            archive_entry_limit: self.archive_entry_limit,
            window: crate::search::campaign::default_window(self.workers),
            memory_budget_mib: self.memory_budget_mib,
            materialize_final_artifacts: self.materialize_final_artifacts,
            run: TwinDragonsCampaignRun,
            mixture: self.mixture,
            retention: self.retention,
            objective_witness_path: self.victory_input_path.clone(),
        }
    }
}

pub fn run_twin_dragons_campaign_checkpointed(
    game: &TwinDragonsGame,
    config: &TwinDragonsCampaignConfig,
    origin: &TwinDragonsCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(TwinDragonsCampaignModeReport, TwinDragonsSnapshotCheckpoint), Box<dyn Error>> {
    run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)
}

pub fn replay_twin_dragons_campaign_checkpointed(
    game: &TwinDragonsGame,
    stream_bytes: &[u8],
    origin_source: Option<&ArchiveSource<TwinDragonsArchiveReport>>,
    origin_checkpoint: Option<&TwinDragonsCampaignCheckpoint>,
) -> Result<(TwinDragonsCampaignModeReport, TwinDragonsSnapshotCheckpoint), Box<dyn Error>> {
    replay_campaign_checkpointed(game, stream_bytes, origin_source, origin_checkpoint)
}

impl crate::film::Filmable for TwinDragonsGame {
    fn film(
        &self,
        input: &TwinDragonsInput,
        tail_frames: u32,
        skip_frames: u64,
        video_output: &mut dyn Write,
        audio_output: &mut dyn Write,
    ) -> Result<crate::film::FilmMetadata, Box<dyn Error>> {
        let mut target = self
            .new_target()
            .map_err(|error| -> Box<dyn Error> { error.into() })?;
        let video =
            target.render_input(input, tail_frames, skip_frames, video_output, audio_output)?;
        Ok(crate::film::FilmMetadata {
            width: video.width,
            height: video.height,
            frames: video.frames,
            skipped_frames: video.skipped_frames,
            audio_sample_rate: video.audio_sample_rate,
            audio_channels: video.audio_channels,
            audio_frames: video.audio_frames,
            input_endpoint: serde_json::to_value(video.input_endpoint)?,
        })
    }
}

impl crate::film::Endpointed for TwinDragonsGame {
    fn headless_endpoint(
        &self,
        input: &TwinDragonsInput,
    ) -> Result<serde_json::Value, Box<dyn Error>> {
        let mut target = self
            .new_target()
            .map_err(|error| -> Box<dyn Error> { error.into() })?;
        target.reset();
        for action in &input.actions {
            target.apply(action);
            if target.exit_kind() != ExitKind::Ok {
                return Err(
                    "the recorded Twin Dragons input crashed during headless replay".into(),
                );
            }
        }
        Ok(serde_json::to_value(target.observe().decoded)?)
    }

    fn input_frames(&self, input: &TwinDragonsInput) -> Result<u64, Box<dyn Error>> {
        Ok(input
            .actions
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum())
    }
}
