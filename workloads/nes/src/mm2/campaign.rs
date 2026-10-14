// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write, path::PathBuf};

use machine::quicknes::QuickNesMachine;

pub use nes_games::mm2::campaign::*;

use crate::{
    mm2::archive::Mm2ArchiveReport,
    search::{
        archive::RetentionPolicy,
        campaign::{CampaignConfig, replay_campaign_checkpointed, run_campaign_checkpointed},
        draw::DrawMixture,
    },
};

pub type Mm2Game<M = QuickNesMachine> = nes_games::mm2::campaign::Mm2Game<M>;
pub type Mm2CampaignOrigin<M = QuickNesMachine> = nes_games::mm2::campaign::Mm2CampaignOrigin<M>;

pub struct Mm2CampaignConfig {
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

impl Mm2CampaignConfig {
    fn generic(&self) -> CampaignConfig<Mm2Game> {
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
            run: Mm2CampaignRun,
            mixture: self.mixture,
            retention: self.retention,
            objective_witness_path: self.victory_input_path.clone(),
        }
    }
}

pub fn run_mm2_campaign_checkpointed(
    game: &Mm2Game,
    config: &Mm2CampaignConfig,
    origin: &Mm2CampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(Mm2CampaignModeReport, Mm2SnapshotCheckpoint), Box<dyn Error>> {
    run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)
}

pub fn replay_mm2_campaign_checkpointed(
    game: &Mm2Game,
    stream_bytes: &[u8],
    origin_report: Option<&Mm2ArchiveReport>,
    origin_checkpoint: Option<&Mm2CampaignCheckpoint>,
) -> Result<(Mm2CampaignModeReport, Mm2SnapshotCheckpoint), Box<dyn Error>> {
    replay_campaign_checkpointed(game, stream_bytes, origin_report, origin_checkpoint)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::{
        mm2::target::{Mm2Route, Mm2Snapshot, Mm2Stage},
        search::campaign::{InputPolicy, Reporting},
    };

    #[test]
    fn the_census_reports_where_the_live_archive_sits_and_what_the_selector_drew() {
        use crate::mm2::target::Mm2MechanicalState;

        let at = |stage, screen, y, health| Mm2MechanicalState {
            stage,
            screen,
            y,
            health,
            ..Mm2MechanicalState::default()
        };
        let cached = [
            (at(8, 4, 10, 28), 3),
            (at(8, 4, 200, 8), 0),
            (at(8, 2, 10, 20), 11),
            (at(2, 4, 10, 5), 1),
        ]
        .map(|(state, selections)| (Mm2Snapshot::for_census_tests(state), selections));
        let census = <Mm2Game>::retained_diagnostics(
            cached
                .iter()
                .map(|(snapshot, selections)| (Some(snapshot), *selections))
                .chain(std::iter::once((None, 9))),
        )
        .expect("census");

        assert_eq!(census["active_entries"], 5);
        assert_eq!(census["missing_snapshots"], 1);
        assert_eq!(census["top_tier"]["castles"], 0);
        let screens = &census["live_entries_by_screen"];
        assert_eq!(screens["8:4"], serde_json::json!([2, 28, 0, 3]));
        assert_eq!(screens["8:2"], serde_json::json!([1, 20, 0, 11]));
        assert_eq!(screens["2:4"], serde_json::json!([1, 5, 0, 1]));
        let rows = &census["live_entries_by_screen_row"];
        assert_eq!(rows["8:4:0"], serde_json::json!([1, 3]));
        assert_eq!(rows["8:4:12"], serde_json::json!([1, 0]));
    }

    #[test]
    fn recorded_policy_set_is_exact_and_game_owned() {
        let game = Mm2Game::new_at_stage(
            &[1, 2, 3],
            Path::new("core.so"),
            &"a".repeat(64),
            Mm2Stage::default(),
        );
        let policies = game.policies(&Mm2CampaignRun);
        let Mm2CampaignRun = game.resolve_recorded(&policies).expect("resolve");
        let mut foreign = policies;
        foreign.insert("stage".to_owned(), "understood-by-search".to_owned());
        assert!(game.resolve_recorded(&foreign).is_err());
    }

    #[test]
    fn selected_stage_is_part_of_recorded_machine_identity() {
        let stage = Mm2Stage::from_number(3).expect("stage");
        let game = Mm2Game::new_at_stage(&[1, 2, 3], Path::new("core.so"), &"a".repeat(64), stage);
        assert_eq!(game.route(), Mm2Route::Stage(stage));
        assert!(
            game.emulator_identity()
                .contains("genesis=mm2-stage-select-v2:3:prefix-sha256=")
        );
    }
}
