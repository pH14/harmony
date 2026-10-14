// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write, path::PathBuf};

use machine::quicknes::QuickNesMachine;

pub use nes_games::metroid::campaign::*;

use crate::{
    metroid::archive::MetroidArchiveReport,
    search::{
        archive::RetentionPolicy,
        campaign::{CampaignConfig, replay_campaign_checkpointed, run_campaign_checkpointed},
        draw::DrawMixture,
    },
};

pub type MetroidGame<M = QuickNesMachine> = nes_games::metroid::campaign::MetroidGame<M>;
pub type MetroidCampaignOrigin<M = QuickNesMachine> =
    nes_games::metroid::campaign::MetroidCampaignOrigin<M>;

pub struct MetroidCampaignConfig {
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

impl MetroidCampaignConfig {
    fn generic(&self) -> CampaignConfig<MetroidGame> {
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
            run: MetroidCampaignRun,
            mixture: self.mixture,
            retention: self.retention,
            objective_witness_path: self.victory_input_path.clone(),
        }
    }
}

pub fn run_metroid_campaign_checkpointed(
    game: &MetroidGame,
    config: &MetroidCampaignConfig,
    origin: &MetroidCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(MetroidCampaignModeReport, MetroidSnapshotCheckpoint), Box<dyn Error>> {
    run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)
}

pub fn replay_metroid_campaign_checkpointed(
    game: &MetroidGame,
    stream_bytes: &[u8],
    origin_report: Option<&MetroidArchiveReport>,
    origin_checkpoint: Option<&MetroidCampaignCheckpoint>,
) -> Result<(MetroidCampaignModeReport, MetroidSnapshotCheckpoint), Box<dyn Error>> {
    replay_campaign_checkpointed(game, stream_bytes, origin_report, origin_checkpoint)
}
