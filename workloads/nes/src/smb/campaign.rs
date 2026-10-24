// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write};

use machine::quicknes::QuickNesMachine;

pub use nes_games::smb::campaign::*;

use crate::{
    nes_backend::{NesBackend, SnapshotState},
    search::archive::RetentionPolicy,
    search::campaign::{
        ArchiveSource, TargetExecution, replay_campaign_checkpointed, run_campaign_checkpointed,
    },
    search::draw::DrawMixture,
    smb::archive::SmbArchiveReport,
    smb::target::SmbInput,
    target::{ExitKind, Target},
};

pub type SmbGame<M = QuickNesMachine, P = Vec<u8>> = nes_games::smb::campaign::SmbGame<M, P>;
pub type SmbCampaignOrigin<M = QuickNesMachine, P = Vec<u8>> =
    nes_games::smb::campaign::SmbCampaignOrigin<M, P>;
pub type SmbCampaignActionResult<M = QuickNesMachine, P = Vec<u8>> =
    nes_games::smb::campaign::SmbCampaignActionResult<M, P>;

pub struct SmbCampaignConfig {
    pub campaign_seed: u64,
    pub workers: u32,
    pub execution_budget: u64,
    pub host: String,
    pub wall_budget: Option<std::time::Duration>,
    pub continue_after_victory: bool,
    pub archive_entry_limit: usize,
    pub window: usize,
    pub memory_budget_mib: Option<usize>,
    pub materialize_final_artifacts: bool,
    pub vocabulary: SmbButtonVocabulary,
    pub terminal: SmbTerminalPredicate,
    pub retention: RetentionPolicy,
    pub mixture: DrawMixture,
    pub victory_input_path: Option<std::path::PathBuf>,
}

impl SmbCampaignConfig {
    fn generic<M, P>(&self) -> GenericCampaignConfig<SmbGame<M, P>>
    where
        M: NesBackend<P>,
        P: SnapshotState,
    {
        GenericCampaignConfig {
            mixture: self.mixture,
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
            stop_rollout_on_objective: !self.continue_after_victory,
            stop_campaign_on_objective: !self.continue_after_victory,
            archive_entry_limit: self.archive_entry_limit,
            window: self.window,
            memory_budget_mib: self.memory_budget_mib,
            materialize_final_artifacts: self.materialize_final_artifacts,
            run: SmbCampaignRun {
                vocabulary: self.vocabulary,
                terminal: Some(self.terminal),
            },
            retention: self.retention,
            objective_witness_path: self.victory_input_path.clone(),
        }
    }
}

pub fn run_smb_campaign<M, P>(
    game: &SmbGame<M, P>,
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin<M, P>,
    stream: &mut dyn Write,
) -> Result<SmbCampaignModeReport, Box<dyn Error>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    run_smb_campaign_with_progress(game, config, origin, stream, None)
}

pub fn run_smb_campaign_with_progress<M, P>(
    game: &SmbGame<M, P>,
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin<M, P>,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<SmbCampaignModeReport, Box<dyn Error>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    run_smb_campaign_checkpointed(game, config, origin, stream, progress).map(|(report, _)| report)
}

pub fn run_smb_campaign_checkpointed<M, P>(
    game: &SmbGame<M, P>,
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin<M, P>,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(SmbCampaignModeReport, SmbSnapshotCheckpoint<P>), Box<dyn Error>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)
}

pub fn replay_smb_campaign<M, P>(
    game: &SmbGame<M, P>,
    stream_bytes: &[u8],
    origin_source: Option<&ArchiveSource<SmbArchiveReport>>,
) -> Result<SmbCampaignModeReport, Box<dyn Error>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    replay_smb_campaign_checkpointed(game, stream_bytes, origin_source, None)
        .map(|(report, _)| report)
}

pub fn replay_smb_campaign_checkpointed<M, P>(
    game: &SmbGame<M, P>,
    stream_bytes: &[u8],
    origin_source: Option<&ArchiveSource<SmbArchiveReport>>,
    origin_checkpoint: Option<&SmbCampaignCheckpoint<P>>,
) -> Result<(SmbCampaignModeReport, SmbSnapshotCheckpoint<P>), Box<dyn Error>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    replay_campaign_checkpointed(game, stream_bytes, origin_source, origin_checkpoint)
}

impl<M, P> crate::film::Endpointed for SmbGame<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    fn headless_endpoint(&self, input: &SmbInput) -> Result<serde_json::Value, Box<dyn Error>> {
        let mut target = self
            .new_target()
            .map_err(|error| -> Box<dyn Error> { error.into() })?;
        target.reset();
        for action in &input.actions {
            target.apply(action);
            if target.exit_kind() != ExitKind::Ok {
                return Err(
                    "the recorded Super Mario Bros input crashed during headless replay".into(),
                );
            }
        }
        Ok(serde_json::to_value(target.observe().decoded)?)
    }

    fn input_frames(&self, input: &SmbInput) -> Result<u64, Box<dyn Error>> {
        Ok(input
            .actions
            .iter()
            .map(|action| u64::from(action.bounded_hold_frames()))
            .sum())
    }
}

#[cfg(test)]
mod tests {
    use crate::search::rollout::Outcome;
    use std::{cell::Cell, collections::BTreeSet};

    use super::{
        ArchiveSource, DrawMixture, SNAPSHOT_CHECKPOINT_FORMAT, SmbButtonVocabulary,
        SmbCampaignActionResult, SmbCampaignCheckpoint, SmbCampaignConfig, SmbCampaignOrigin,
        SmbCampaignProgressRecord, SmbCampaignRun, SmbCampaignStreamRecord, SmbGame,
        SmbSnapshotCheckpoint, SmbSnapshotCheckpointEntry, SmbTerminalPredicate,
        derive_selection_seed,
    };
    use crate::search::campaign::{Evaluation, InputPolicy, TargetExecution, default_window};
    use crate::search::draw_tables::{DEFAULT_DRAW_TABLE_PARAMETERS, DrawTables};
    use crate::{
        chord::CHANGE_ONE_CONTROL_IDENTIFIER,
        smb::archive::SmbArchiveReport,
        smb::target::{
            ButtonChord, SmbInput, SmbMechanicalState, SmbMilestones, SmbObservations,
            SmbProgressWatermark, SmbTarget,
        },
        target::Target,
    };

    fn synthetic_nrom() -> Vec<u8> {
        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg = &mut rom[16..16 + (16 * 1024)];
        prg.fill(0xea);
        prg[..3].copy_from_slice(&[0x4c, 0x00, 0x80]);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        rom
    }

    fn test_game(rom: &[u8]) -> SmbGame {
        SmbGame::loopback_for_tests(rom)
    }

    #[test]
    fn adaptive_input_modes_round_trip_through_ordinary_replay() {
        use crate::search::campaign::{
            CampaignExecutionOptions, replay_campaign_checkpointed,
            run_campaign_checkpointed_with_options,
        };
        use crate::search::draw::InputMode;
        for mode in [
            InputMode::Current,
            InputMode::Chaining,
            InputMode::Sticky,
            InputMode::StickyChaining,
        ] {
            let rom = synthetic_nrom();
            let game = test_game(&rom);
            let mut config = genesis_config(73, 1, 48).generic();
            config.window = 1;
            config.mixture = DrawMixture::EnergySplice { scale: 6 };
            config.retention = crate::search::archive::RetentionPolicy::Unprobed;
            let mut stream = Vec::new();
            let live = run_campaign_checkpointed_with_options(
                &game,
                &config,
                &SmbCampaignOrigin::Genesis,
                &mut stream,
                None,
                CampaignExecutionOptions {
                    input_mode: mode,
                    ..Default::default()
                },
            )
            .unwrap();
            let replay = replay_campaign_checkpointed(&game, &stream, None, None)
                .unwrap_or_else(|error| panic!("{mode:?}: {error}"));
            assert_eq!(live, replay, "{mode:?}");
            if mode != InputMode::Current {
                let text = String::from_utf8(stream).unwrap();
                assert!(
                    text.contains("input_decision"),
                    "{mode:?} never selected an experimental input"
                );
                let mut records: Vec<serde_json::Value> = text
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
                let record = records
                    .iter_mut()
                    .find(|record| record.get("input_decision").is_some())
                    .unwrap();
                let before = record["input_decision"]["sticky"].as_bool().unwrap();
                record["input_decision"]["sticky"] = serde_json::Value::Bool(!before);
                let corrupt = records
                    .iter()
                    .map(|record| serde_json::to_string(record).unwrap())
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n";
                assert!(
                    replay_campaign_checkpointed(&game, corrupt.as_bytes(), None, None).is_err()
                );
            }
        }
    }

    #[test]
    fn all_four_input_modes_execute_and_admit_one_job() {
        use crate::search::archive::RetentionPolicy;
        use crate::search::campaign::{CampaignCoordinator, CampaignExecutionOptions};
        for (sticky, chaining) in [(false, false), (false, true), (true, false), (true, true)] {
            let rom = synthetic_nrom();
            let game = test_game(&rom);
            let mut config = genesis_config(73, 1, 1).generic();
            config.window = 1;
            config.retention = RetentionPolicy::Unprobed;
            config.materialize_final_artifacts = false;
            let mut stream = Vec::new();
            let mut coordinator = CampaignCoordinator::start(
                &game,
                &config,
                &SmbCampaignOrigin::Genesis,
                &mut stream,
                None,
                CampaignExecutionOptions::default(),
            )
            .unwrap();
            coordinator.reserve_next().unwrap();
            let mut job = coordinator.plan(1).pop().unwrap();
            job.suffix = vec![ButtonChord::new(0x81, 4), ButtonChord::new(0x40, 5)];
            let mut target = game.new_target().unwrap();
            let (outcome, trace) = coordinator
                .run_input_job(&mut target, &job, sticky, chaining, 480)
                .unwrap();
            if !chaining {
                assert_eq!(outcome.execution_work, 9);
                assert_eq!(trace.blocks.len(), 1);
            } else {
                assert!(trace.blocks[0].work >= 120);
                assert!(
                    trace
                        .blocks
                        .iter()
                        .all(|block| block.before.is_some() && block.after.is_some())
                );
            }
            assert!(!outcome.result.actions.is_empty());
            coordinator.admit(outcome).unwrap();
            let (report, _) = coordinator.finish(Vec::new()).unwrap();
            assert_eq!(report.executions_completed, 1);
            let text = String::from_utf8(stream).unwrap();
            assert_eq!(
                text.lines()
                    .filter(|line| line.contains("\"event\":\"job\""))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn sticky_adapter_preserves_frames_across_chord_and_block_boundaries() {
        use crate::search::sticky_input::StickyInput;
        use std::num::NonZeroU64;
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let run = SmbCampaignRun {
            vocabulary: Default::default(),
            terminal: None,
        };
        let previous = ButtonChord::new(0x81, 3);
        let (start, mask, support) = game.sticky_controller(&run, Some(&previous)).unwrap();
        assert_eq!(start, previous.buttons);
        assert_eq!(mask & 0x0c, 0);
        let mut input = StickyInput::new(73, start, mask, support);
        let mut expected = StickyInput::new(73, start, mask, support);
        let mut actual = Vec::new();
        for length in [1, 17, 120, 900, 2] {
            for action in game
                .sticky_actions(&run, &mut input, NonZeroU64::new(length).unwrap())
                .unwrap()
            {
                assert!((1..=120).contains(&action.hold_frames));
                actual.extend(std::iter::repeat_n(
                    action.buttons,
                    usize::from(action.hold_frames),
                ));
            }
        }
        assert_eq!(
            actual,
            (0..1040).map(|_| expected.next_frame()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn selected_symbols_keep_buttons_but_do_not_inherit_short_holds() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let run = SmbCampaignRun {
            vocabulary: Default::default(),
            terminal: None,
        };
        let original: Vec<_> = (0..4096)
            .map(|i| ButtonChord::new(if i % 2 == 0 { 0x81 } else { 0x40 }, 3))
            .collect();
        let mut suffix = original.clone();
        game.refresh_selected_input_timing(&run, 12345, &mut suffix)
            .unwrap();
        assert!(
            suffix
                .iter()
                .zip(&original)
                .all(|(a, b)| a.buttons == b.buttons)
        );
        let mut holds: Vec<_> = suffix.iter().map(|a| a.hold_frames).collect();
        holds.sort_unstable();
        assert!((55..=65).contains(&holds[holds.len() / 2]));
        assert_eq!(holds[0], 2);
        assert_eq!(*holds.last().unwrap(), 120);
        let mut replay = original;
        game.refresh_selected_input_timing(&run, 12345, &mut replay)
            .unwrap();
        assert_eq!(suffix, replay);
    }

    #[test]
    fn native_policy_rejects_a_changed_prepared_execution() {
        let first = SmbGame::new(&[0], std::path::Path::new("core.so"), "core", "first");
        let second = SmbGame::new(&[0], std::path::Path::new("core.so"), "core", "second");
        let run = SmbCampaignRun {
            vocabulary: SmbButtonVocabulary::default(),
            terminal: None,
        };
        assert!(
            first
                .emulator_identity()
                .contains("prepared-execution-sha256=first")
        );
        assert!(second.resolve_recorded(&first.policies(&run)).is_err());
    }

    #[cfg(all(
        feature = "consonance",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ))]
    #[test]
    fn consonance_policy_binds_the_admitted_identity_before_guest_replay() {
        let expected = "consonance-whole-vm-v3;session=admitted-test-session";
        let game = SmbGame::<
            machine::consonance::ConsonanceMachine,
            machine::consonance::ConsonancePortable,
        >::with_targets(
            &synthetic_nrom(),
            format!("{expected};result_digest=semantic-postcard-1.1.3-sha256-hex-v1"),
            super::CONSONANCE_SNAPSHOT_CHECKPOINT_FORMAT,
            Box::new(|| Err("the policy check never builds a target".to_owned())),
        );
        assert!(game.emulator_identity().starts_with(expected));

        let run = SmbCampaignRun {
            vocabulary: SmbButtonVocabulary::default(),
            terminal: None,
        };
        let mut policies = game.policies(&run);
        assert!(game.resolve_recorded(&policies).is_ok());
        policies.insert(
            super::EMULATOR_BACKEND_FIELD.to_owned(),
            "another loaded executable".to_owned(),
        );
        assert!(game.resolve_recorded(&policies).is_err());
    }

    fn run_smb_campaign(
        rom: &[u8],
        config: &SmbCampaignConfig,
        origin: &SmbCampaignOrigin,
        stream: &mut dyn std::io::Write,
    ) -> Result<super::SmbCampaignModeReport, Box<dyn std::error::Error>> {
        super::run_smb_campaign(&test_game(rom), config, origin, stream)
    }

    fn run_smb_campaign_with_progress(
        rom: &[u8],
        config: &SmbCampaignConfig,
        origin: &SmbCampaignOrigin,
        stream: &mut dyn std::io::Write,
        progress: Option<&mut dyn std::io::Write>,
    ) -> Result<super::SmbCampaignModeReport, Box<dyn std::error::Error>> {
        super::run_smb_campaign_with_progress(&test_game(rom), config, origin, stream, progress)
    }

    fn run_smb_campaign_checkpointed(
        rom: &[u8],
        config: &SmbCampaignConfig,
        origin: &SmbCampaignOrigin,
        stream: &mut dyn std::io::Write,
        progress: Option<&mut dyn std::io::Write>,
    ) -> Result<(super::SmbCampaignModeReport, SmbSnapshotCheckpoint), Box<dyn std::error::Error>>
    {
        super::run_smb_campaign_checkpointed(&test_game(rom), config, origin, stream, progress)
    }

    fn replay_smb_campaign(
        rom: &[u8],
        stream_bytes: &[u8],
        origin_source: Option<&ArchiveSource<SmbArchiveReport>>,
    ) -> Result<super::SmbCampaignModeReport, Box<dyn std::error::Error>> {
        super::replay_smb_campaign(&test_game(rom), stream_bytes, origin_source)
    }

    fn replay_smb_campaign_checkpointed(
        rom: &[u8],
        stream_bytes: &[u8],
        origin_source: Option<&ArchiveSource<SmbArchiveReport>>,
        origin_checkpoint: Option<&SmbCampaignCheckpoint>,
    ) -> Result<(super::SmbCampaignModeReport, SmbSnapshotCheckpoint), Box<dyn std::error::Error>>
    {
        super::replay_smb_campaign_checkpointed(
            &test_game(rom),
            stream_bytes,
            origin_source,
            origin_checkpoint,
        )
    }

    fn genesis_config(
        campaign_seed: u64,
        workers: u32,
        execution_budget: u64,
    ) -> SmbCampaignConfig {
        SmbCampaignConfig {
            vocabulary: SmbButtonVocabulary::default(),
            terminal: super::SmbTerminalPredicate::GameVictory,
            campaign_seed,
            workers,
            execution_budget,
            host: "unit-test".to_owned(),
            wall_budget: None,
            continue_after_victory: false,
            archive_entry_limit: 32_768,
            window: default_window(workers),
            memory_budget_mib: None,
            materialize_final_artifacts: true,
            retention: crate::search::archive::RetentionPolicy::ProbeAtAdmission,
            mixture: DrawMixture::BiasedHalf,
            victory_input_path: None,
        }
    }

    #[test]
    fn selection_seed_derivation_is_stable() {
        let seed = derive_selection_seed(0x5eed_ca00);
        assert_eq!(seed, derive_selection_seed(0x5eed_ca00));
        assert_ne!(seed, derive_selection_seed(0x5eed_ca01));
    }

    fn evidence_action(milestones: SmbMilestones) -> SmbCampaignActionResult {
        SmbCampaignActionResult {
            action: ButtonChord::new(0x01, 4),
            observations: vec![SmbObservations {
                frame_count: 1,
                wram: Vec::new(),
                decoded: SmbMechanicalState {
                    world: 1,
                    level: 2,
                    progress: 33,
                    ..SmbMechanicalState::default()
                },
                milestones,
                dead: false,
            }],
            milestones,
            outcome: Outcome::default(),
            candidate: None,
        }
    }

    #[test]
    fn action_evidence_materializes_inputs_only_for_real_discoveries() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let run = |milestones| {
            let mut evidence = super::SmbCampaignEvidence::default();
            let calls = Cell::new(0);
            game.merge_action_evidence(&mut evidence, &evidence_action(milestones), 7, || {
                calls.set(calls.get() + 1);
                Ok(SmbInput {
                    actions: vec![ButtonChord::new(0x01, 4)],
                })
            })
            .expect("merge action evidence");
            (evidence, calls.get())
        };

        let (empty, empty_calls) = run(SmbMilestones::default());
        assert_eq!(empty_calls, 0);
        assert_eq!(
            empty.watermark,
            SmbProgressWatermark {
                world: 1,
                level: 2,
                progress: 33
            }
        );
        assert_eq!(empty.champion_milestones, SmbMilestones::default());

        let first = SmbMilestones {
            max_1_1_scroll_bucket: 1,
            ..SmbMilestones::default()
        };
        let (progress, progress_calls) = run(first);
        assert_eq!(progress_calls, 1);
        assert_eq!(progress.first_reached.progress_into_1_1, Some(7));
        assert_eq!(progress.champion_milestones, first);
        assert_eq!(progress.champion_input.actions.len(), 1);

        for milestones in [
            SmbMilestones {
                reached_1_1_flag: true,
                ..SmbMilestones::default()
            },
            SmbMilestones {
                reached_1_2: true,
                ..SmbMilestones::default()
            },
            SmbMilestones {
                reached_onward: true,
                ..SmbMilestones::default()
            },
        ] {
            let (evidence, calls) = run(milestones);
            assert_eq!(calls, 1);
            assert_eq!(evidence.champion_milestones, milestones);
        }

        let reigning = SmbMilestones {
            max_1_1_scroll_bucket: u16::MAX,
            reached_1_1_flag: true,
            reached_1_2: true,
            reached_onward: true,
        };
        for milestones in [
            SmbMilestones {
                max_1_1_scroll_bucket: 1,
                ..SmbMilestones::default()
            },
            SmbMilestones {
                reached_1_1_flag: true,
                ..SmbMilestones::default()
            },
            SmbMilestones {
                reached_1_2: true,
                ..SmbMilestones::default()
            },
            SmbMilestones {
                reached_onward: true,
                ..SmbMilestones::default()
            },
        ] {
            let mut evidence = super::SmbCampaignEvidence {
                champion_milestones: reigning,
                ..super::SmbCampaignEvidence::default()
            };
            let calls = Cell::new(0);
            game.merge_action_evidence(&mut evidence, &evidence_action(milestones), 9, || {
                calls.set(calls.get() + 1);
                Ok(SmbInput {
                    actions: vec![ButtonChord::new(0x02, 4)],
                })
            })
            .expect("merge first-input-only evidence");
            assert_eq!(calls.get(), 1);
            assert_eq!(evidence.champion_milestones, reigning);
        }
    }

    #[test]
    fn worker_keys_preserve_recorded_room_assignment() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let mut target = game.new_target().expect("load target");
        target.reset();
        target.poke_wram(0x074e, 7).unwrap();
        target.poke_wram(0x074f, 9).unwrap();
        let origin = target.snapshot().expect("snapshot room");
        let result = game
            .execute_job(
                &SmbCampaignRun {
                    vocabulary: SmbButtonVocabulary::default(),
                    terminal: Some(SmbTerminalPredicate::GameVictory),
                },
                &mut target,
                &origin,
                &[],
                SmbMilestones::default(),
                &[ButtonChord::new(0x01, 1)],
                crate::search::archive::RetentionPolicy::Unprobed,
                true,
            )
            .expect("execute room action");
        let candidate = result.actions[0]
            .candidate
            .as_ref()
            .expect("live candidate");
        assert_eq!(candidate.key.room, [0; 3]);
        let completed = game
            .complete_candidate_key(candidate.key, &candidate.snapshot)
            .expect("complete arrival room");
        assert_eq!(completed, game.current_key(&target).expect("current room"));
        assert_eq!(&completed.room[..2], &[7, 9]);
        assert_ne!(
            postcard::to_stdvec(&candidate.key).expect("recorded worker key"),
            postcard::to_stdvec(&completed).expect("completed archive key")
        );
    }

    #[test]
    fn job_execution_is_pure_across_target_instances() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let mut first = game.new_target().expect("load first target");
        let mut second = game.new_target().expect("load second target");
        first.reset();
        first.apply(&ButtonChord::new(0x81, 12));
        let snapshot = first.snapshot().expect("snapshot prefix");
        let tables =
            DrawTables::<ButtonChord>::new(DEFAULT_DRAW_TABLE_PARAMETERS).expect("draw tables");
        let suffix = game
            .expand_suffix(
                &SmbCampaignRun {
                    vocabulary: SmbButtonVocabulary::default(),
                    terminal: None,
                },
                &tables,
                crate::search::draw::MixtureDraw {
                    mixture: DrawMixture::BiasedHalf,
                    weight: 128,
                    splice_weight: 0,
                },
                0x5eed_ca02,
                None,
            )
            .expect("draw a suffix");
        first.apply(&ButtonChord::new(0x02, 30));
        let run = SmbCampaignRun {
            vocabulary: SmbButtonVocabulary::default(),
            terminal: Some(SmbTerminalPredicate::GameVictory),
        };
        let on_first = game
            .execute_job(
                &run,
                &mut first,
                &snapshot,
                &[],
                SmbMilestones::default(),
                &suffix,
                crate::search::archive::RetentionPolicy::ProbeAtAdmission,
                false,
            )
            .expect("execute job on first instance");
        let on_second = game
            .execute_job(
                &run,
                &mut second,
                &snapshot,
                &[],
                SmbMilestones::default(),
                &suffix,
                crate::search::archive::RetentionPolicy::ProbeAtAdmission,
                false,
            )
            .expect("execute job on second instance");
        assert_eq!(on_first, on_second);
    }

    #[test]
    fn a_job_from_a_won_snapshot_executes_nothing() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let mut target = game.new_target().expect("load target");
        target.reset();
        target.poke_wram(0x0770, 2).unwrap();
        target.poke_wram(0x075f, 7).unwrap();
        let run = SmbCampaignRun {
            vocabulary: SmbButtonVocabulary::default(),
            terminal: Some(SmbTerminalPredicate::GameVictory),
        };
        assert_eq!(
            game.execution_disposition(&target),
            crate::search::rollout::ExecutionDisposition::Terminal
        );
        assert_eq!(
            game.rollout_outcome(&run, &target)
                .expect("won outcome")
                .disposition,
            game.execution_disposition(&target)
        );
        let won = target.snapshot().expect("snapshot won state");
        let result = game
            .execute_job(
                &run,
                &mut target,
                &won,
                &[],
                SmbMilestones::default(),
                &[ButtonChord::new(0x01, 4)],
                crate::search::archive::RetentionPolicy::ProbeAtAdmission,
                true,
            )
            .expect("execute job");
        assert!(result.actions.is_empty());
    }

    #[test]
    fn terminal_predicate_is_mechanical_and_round_trips() {
        let rom = synthetic_nrom();
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("load target");
        target.reset();
        let transition = SmbTerminalPredicate::LevelTransition { world: 0, level: 0 };
        assert!(!transition.reached(&target).expect("initial terminal state"));
        target.poke_wram(0x075c, 1).unwrap();
        assert!(transition.reached(&target).expect("changed terminal state"));
        assert_eq!(
            SmbTerminalPredicate::from_identifier(&transition.identifier())
                .expect("terminal predicate round trip"),
            transition
        );
        assert!(SmbTerminalPredicate::from_identifier("level_transition:0,0,1").is_err());
        assert!(SmbTerminalPredicate::from_identifier("level_transition:256,0").is_err());
        assert!(SmbTerminalPredicate::from_identifier("coordinates:0,0").is_err());
    }

    #[test]
    fn snapshot_root_replays_and_binds_its_identity() {
        use sha2::{Digest, Sha256};

        let rom = synthetic_nrom();
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("load target");
        target.reset();
        let snapshot = target.snapshot().expect("snapshot root");
        let snapshots = SmbSnapshotCheckpoint {
            format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
            entries: vec![SmbSnapshotCheckpointEntry { id: 0, snapshot }],
        };
        let checkpoint_bytes = snapshots.to_bytes().expect("encode root checkpoint");
        let checkpoint = SmbCampaignCheckpoint {
            path: "fixture-neutral-00".to_owned(),
            file_sha256: format!("{:x}", Sha256::digest(&checkpoint_bytes)),
            snapshots,
        };
        let config = SmbCampaignConfig {
            terminal: SmbTerminalPredicate::LevelTransition { world: 0, level: 0 },
            ..genesis_config(0x5eed_ca20, 2, 4)
        };
        let origin = SmbCampaignOrigin::SnapshotRoot {
            checkpoint: checkpoint.clone(),
        };
        let mut stream = Vec::new();
        let (live, live_checkpoint) =
            run_smb_campaign_checkpointed(&rom, &config, &origin, &mut stream, None)
                .expect("snapshot-root campaign");
        assert_eq!(live.origin.kind, "snapshot_root");
        assert_eq!(live.origin.resume_actions, 0);
        assert_eq!(live.resume_policy, "snapshot_root");
        assert_eq!(live.archive.entries[0].id, 0);
        let (replay, replay_checkpoint) =
            replay_smb_campaign_checkpointed(&rom, &stream, None, Some(&checkpoint))
                .expect("replay snapshot-root campaign");
        assert_eq!(live, replay);
        assert_eq!(live_checkpoint, replay_checkpoint);
        assert!(replay_smb_campaign_checkpointed(&rom, &stream, None, None).is_err());

        let mut wrong_path = checkpoint.clone();
        wrong_path.path.push_str("-wrong");
        assert!(replay_smb_campaign_checkpointed(&rom, &stream, None, Some(&wrong_path)).is_err());
        let mut wrong_hash = checkpoint.clone();
        let replacement = if wrong_hash.file_sha256.starts_with('0') {
            "1"
        } else {
            "0"
        };
        wrong_hash.file_sha256.replace_range(..1, replacement);
        assert!(replay_smb_campaign_checkpointed(&rom, &stream, None, Some(&wrong_hash)).is_err());
    }

    #[test]
    fn snapshot_root_accepts_a_terminal_objective_checkpoint() {
        use sha2::{Digest, Sha256};

        let rom = synthetic_nrom();
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("load target");
        target.reset();
        target.poke_wram(0x075c, 1).unwrap();
        let snapshot = target.snapshot().expect("snapshot terminal root");
        let snapshots = SmbSnapshotCheckpoint {
            format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
            entries: vec![SmbSnapshotCheckpointEntry { id: 0, snapshot }],
        };
        let checkpoint = SmbCampaignCheckpoint {
            path: "fixture-terminal-00".to_owned(),
            file_sha256: format!(
                "{:x}",
                Sha256::digest(snapshots.to_bytes().expect("encode terminal root"))
            ),
            snapshots,
        };
        let config = SmbCampaignConfig {
            terminal: SmbTerminalPredicate::LevelTransition { world: 0, level: 0 },
            ..genesis_config(0x5eed_ca21, 1, 1)
        };
        let origin = SmbCampaignOrigin::SnapshotRoot { checkpoint };
        let mut stream = Vec::new();
        let (report, _) = run_smb_campaign_checkpointed(&rom, &config, &origin, &mut stream, None)
            .expect("terminal objective checkpoint is a valid zero-job campaign");
        assert_eq!(report.objectives_reached, 1);
        assert_eq!(report.terminal_objectives, 1);
        assert_eq!(report.executions_completed, 0);
    }

    #[test]
    fn twelve_worker_windowed_streams_are_repeatable_and_replay_exactly() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca21, 12, 24);
        config.retention = crate::search::archive::RetentionPolicy::Unprobed;
        let mut first_stream = Vec::new();
        let first = run_smb_campaign(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut first_stream,
        )
        .expect("first 12-worker campaign");
        let mut second_stream = Vec::new();
        let second = run_smb_campaign(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut second_stream,
        )
        .expect("second 12-worker campaign");
        let first_replay =
            replay_smb_campaign(&rom, &first_stream, None).expect("first windowed stream replays");
        let second_replay = replay_smb_campaign(&rom, &second_stream, None)
            .expect("second windowed stream replays");
        assert_eq!(first_stream, second_stream);
        assert_eq!(first, second);
        assert_eq!(first, first_replay);
        assert_eq!(second, second_replay);
        assert!(
            std::str::from_utf8(&first_stream)
                .expect("stream utf-8")
                .lines()
                .next()
                .expect("stream header")
                .contains("\"schedule_policy\":\"planned_finish_window_12_v6\"")
        );
    }

    #[test]
    fn a_run_without_a_victory_reports_no_first_victory_counters() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca43, 2, 32);
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign without a victory");
        assert_eq!(live.objectives_reached, 0);
        assert_eq!(live.work_to_first_objective, None);
        assert_eq!(live.executions_to_first_objective, None);
        assert!(live.executions_completed > 0);
        let json = serde_json::to_string(&live).expect("serialize report");
        for field in ["frames_to_first_victory", "executions_to_first_victory"] {
            assert!(
                !json.contains(field),
                "a run without a victory must not add {field} to its report"
            );
        }
    }

    #[test]
    fn a_zero_reservation_window_is_refused_by_the_public_campaign_entry() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca40, 2, 8);
        config.window = 0;
        let mut stream = Vec::new();
        let error = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect_err("a zero window is refused");
        assert!(
            error.to_string().contains("admission window"),
            "unexpected error: {error}"
        );
        assert!(
            stream.is_empty(),
            "a refused run must not write a stream header"
        );
    }

    #[test]
    fn a_live_window_of_sixty_four_records_and_replays_as_a_window() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca41, 2, 256);
        config.window = 128;
        config.memory_budget_mib = Some(4);
        let mut stream = Vec::new();
        let (live, live_checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("window-64 live campaign");
        let recorded = String::from_utf8(stream).expect("stream is utf-8");
        assert!(
            recorded
                .lines()
                .next()
                .expect("stream header")
                .contains("\"schedule_policy\":\"planned_finish_window_128_v6\""),
            "unexpected header: {}",
            recorded.lines().next().unwrap_or_default()
        );
        let (replay, replay_checkpoint) =
            replay_smb_campaign_checkpointed(&rom, recorded.as_bytes(), None, None)
                .expect("window-64 stream replays off the windowed path");
        assert_eq!(live, replay);
        assert_eq!(live_checkpoint, replay_checkpoint);
        let legacy_tagged = recorded.replacen(
            "planned_finish_window_128_v6",
            "deterministic_window_128_v4",
            1,
        );
        assert!(
            replay_smb_campaign_checkpointed(&rom, legacy_tagged.as_bytes(), None, None).is_err(),
            "the legacy path must not silently accept a live window-64 stream"
        );
    }

    #[test]
    fn obsolete_campaign_policies_are_rejected_before_replay() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca44, 2, 128);
        config.retention = crate::search::archive::RetentionPolicy::Unprobed;
        config.memory_budget_mib = Some(4);
        config.archive_entry_limit = 1;
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("budgeted live campaign");
        assert!(
            live.history_compactions > 0 && live.historical_entries_dropped > 0,
            "the budget must bind for this stream to be maintenance-sensitive"
        );
        let recorded = String::from_utf8(stream).expect("stream is utf-8");
        replay_smb_campaign(&rom, recorded.as_bytes(), None).expect("its own namespace replays");

        for historical in [
            recorded.replacen(
                "planned_finish_window_2_v6",
                "planned_finish_window_2_v5",
                1,
            ),
            recorded.replacen(
                "mechanical_watermark_bounded_1024_v2",
                "mechanical_watermark_v1",
                1,
            ),
        ] {
            let error = replay_smb_campaign(&rom, historical.as_bytes(), None)
                .expect_err("an obsolete campaign policy is refused");
            let message = error.to_string();
            assert!(
                message.contains("schedule policy") || message.contains("progress policy"),
                "unexpected obsolete policy error: {message}"
            );
        }

        let without_schedule =
            recorded.replacen("\"schedule_policy\":\"planned_finish_window_2_v6\",", "", 1);
        assert!(
            replay_smb_campaign(&rom, without_schedule.as_bytes(), None).is_err(),
            "a recording without the current schedule policy is refused"
        );
        let without_progress = recorded.replacen(
            "\"progress_policy\":\"mechanical_watermark_bounded_1024_v2\",",
            "",
            1,
        );
        assert!(
            replay_smb_campaign(&rom, without_progress.as_bytes(), None).is_err(),
            "a recording without the current progress policy is refused"
        );
        let fresh_chords = recorded.replacen(CHANGE_ONE_CONTROL_IDENTIFIER, "fresh", 1);
        assert!(
            replay_smb_campaign(&rom, fresh_chords.as_bytes(), None).is_err(),
            "a recording with another chord draw is refused"
        );
    }

    #[test]
    fn snapshot_root_rejects_malformed_checkpoint_shapes() {
        use sha2::{Digest, Sha256};

        let rom = synthetic_nrom();
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("load target");
        target.reset();
        let snapshot = target.snapshot().expect("snapshot root");
        let valid_snapshots = SmbSnapshotCheckpoint {
            format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
            entries: vec![SmbSnapshotCheckpointEntry {
                id: 0,
                snapshot: snapshot.clone(),
            }],
        };
        let valid = SmbCampaignCheckpoint {
            path: "fixture-neutral-01".to_owned(),
            file_sha256: format!(
                "{:x}",
                Sha256::digest(valid_snapshots.to_bytes().expect("encode valid root"))
            ),
            snapshots: valid_snapshots,
        };
        for malformed in [
            SmbCampaignCheckpoint {
                path: String::new(),
                ..valid.clone()
            },
            SmbCampaignCheckpoint {
                file_sha256: "0".repeat(63),
                ..valid.clone()
            },
            SmbCampaignCheckpoint {
                file_sha256: "0".repeat(64),
                ..valid.clone()
            },
            SmbCampaignCheckpoint {
                snapshots: SmbSnapshotCheckpoint {
                    format: "wrong".to_owned(),
                    entries: valid.snapshots.entries.clone(),
                },
                ..valid.clone()
            },
            SmbCampaignCheckpoint {
                snapshots: SmbSnapshotCheckpoint {
                    format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
                    entries: Vec::new(),
                },
                ..valid.clone()
            },
            SmbCampaignCheckpoint {
                snapshots: SmbSnapshotCheckpoint {
                    format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
                    entries: vec![SmbSnapshotCheckpointEntry {
                        id: 1,
                        snapshot: snapshot.clone(),
                    }],
                },
                ..valid.clone()
            },
            SmbCampaignCheckpoint {
                snapshots: SmbSnapshotCheckpoint {
                    format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
                    entries: vec![
                        SmbSnapshotCheckpointEntry {
                            id: 0,
                            snapshot: snapshot.clone(),
                        },
                        SmbSnapshotCheckpointEntry { id: 1, snapshot },
                    ],
                },
                ..valid.clone()
            },
        ] {
            let config = genesis_config(0x5eed_ca23, 1, 1);
            let origin = SmbCampaignOrigin::SnapshotRoot {
                checkpoint: malformed,
            };
            assert!(
                run_smb_campaign_checkpointed(&rom, &config, &origin, &mut Vec::new(), None)
                    .is_err()
            );
        }
    }

    #[test]
    fn live_campaign_replays_byte_identically() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca03, 4, 32);
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign");
        assert_eq!(live.executions_completed, 32);
        assert_eq!(
            live.telemetry
                .workers
                .iter()
                .map(|worker| worker.jobs)
                .sum::<u64>(),
            32
        );
        assert_eq!(live.objectives_reached, 0);
        assert_eq!(live.objective_witness, None);
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        let header = text.lines().next().expect("header");
        for identifier in [
            crate::search::archive::SELECTOR_IDENTIFIER,
            "probe_at_admission",
            "fewest_frames_in_level",
            "whole_tree",
            "nes_pressable_36",
            "frozen_area_span",
            crate::smb::campaign::DURATION_IDENTIFIER,
        ] {
            assert!(header.contains(identifier), "header lacks {identifier}");
        }
        let records = text.lines().skip(1).collect::<Vec<_>>();
        let end: SmbCampaignStreamRecord = serde_json::from_str(records.last().unwrap()).unwrap();
        let SmbCampaignStreamRecord::End(end) = end else {
            panic!("campaign stream needs a terminal record");
        };
        assert_eq!(end.jobs, 32);
        assert_eq!(end.executions_completed, 32);
        for line in &records[..records.len() - 1] {
            assert!(line.contains("\"selector\""));
            assert!(
                line.contains("\"tiers\"")
                    || line.contains("\"continuation\"")
                    || line.contains("\"recent\"")
            );
        }
        let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay recorded campaign");
        assert_eq!(live, replayed);
        let live_bytes = serde_json::to_vec_pretty(&live).expect("serialize live report");
        let replay_bytes = serde_json::to_vec_pretty(&replayed).expect("serialize replayed report");
        assert_eq!(live_bytes, replay_bytes);
        let accounting = live.archive.selector;
        assert!(accounting.recent_selections.unwrap_or(0) > 0);
        assert_eq!(
            accounting
                .cell_selections
                .checked_add(accounting.continuation_selections.unwrap_or(0)),
            live.executions_completed
                .checked_add(live.duplicates_skipped)
        );
        assert_eq!(
            accounting.tier_draws_by_rank.iter().sum::<u64>(),
            accounting.cell_selections
        );
    }

    #[test]
    fn budgeted_64_entry_campaign_replays_exactly() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca34, 4, 2_048);
        config.retention = crate::search::archive::RetentionPolicy::Unprobed;
        config.memory_budget_mib = Some(4);
        config.archive_entry_limit = 64;
        let mut stream = Vec::new();
        let (live, live_checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("budgeted live campaign");
        assert_eq!(live.executions_completed, 2_048);
        assert_eq!(live.memory_budget_mib, Some(4));
        assert!(live.resident_memory_bytes <= 4 * 1024 * 1024);
        assert!(live.archive.retained > 1);
        assert!(live.history_compactions > 0);
        assert!(live.historical_entries_dropped > 0);
        assert!(live.live_entries < usize::try_from(live.archive.retained).unwrap_or(usize::MAX));
        assert!(live_checkpoint.entries.len() <= live.archive.entries.len());

        let (replay, replay_checkpoint) =
            replay_smb_campaign_checkpointed(&rom, &stream, None, None)
                .expect("replay budgeted campaign");
        assert_eq!(live, replay);
        assert_eq!(live_checkpoint, replay_checkpoint);
        let anchor = live
            .archive
            .entries
            .iter()
            .find(|entry| entry.id == 0)
            .expect("genesis liveness anchor remains retained");
        assert!(
            anchor
                .selector
                .as_ref()
                .is_some_and(|selector| selector.selected > 0 && selector.productive > 0)
        );
    }

    #[test]
    fn budgeted_single_entry_campaign_reactivates_displaced_anchor() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca32, 1, 128);
        config.retention = crate::search::archive::RetentionPolicy::Unprobed;
        config.memory_budget_mib = Some(4);
        config.archive_entry_limit = 1;
        let mut stream = Vec::new();
        let (live, checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("single-entry bounded campaign");
        assert!(live.history_compactions > 0);
        assert!(live.historical_entries_dropped > 0);
        let (replay, replay_checkpoint) =
            replay_smb_campaign_checkpointed(&rom, &stream, None, None)
                .expect("single-entry bounded replay");
        assert_eq!(live, replay);
        assert_eq!(checkpoint, replay_checkpoint);
    }

    #[test]
    fn recorded_prefix_rebuilds_without_a_live_checkpoint() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca04, 4, 32);
        let mut stream = Vec::new();
        run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign");
        let text = std::str::from_utf8(&stream).expect("stream is utf-8");
        let lines = text.lines().collect::<Vec<_>>();
        let mut reservations = BTreeSet::new();
        let end = (1..lines.len())
            .find(|&index| {
                if let SmbCampaignStreamRecord::Job(job) =
                    serde_json::from_str::<SmbCampaignStreamRecord>(lines[index])
                        .expect("decode prefix record")
                {
                    reservations.insert(job.reservation);
                }
                index >= 8
                    && reservations
                        .iter()
                        .copied()
                        .eq(0..reservations.len() as u64)
            })
            .expect("a prefix admits every reservation below its last");
        let prefix_lines = &lines[..=end];
        let expected_executions =
            u64::try_from(reservations.len()).expect("short prefix count fits u64");
        let prefix = format!("{}\n", prefix_lines.join("\n"));
        let (rebuilt, checkpoint) = crate::search::campaign::replay_campaign_prefix_checkpointed(
            &test_game(&rom),
            prefix.as_bytes(),
            None,
            None,
        )
        .expect("rebuild recorded prefix");
        assert_eq!(rebuilt.mode, "campaign-prefix");
        assert!(expected_executions > 0);
        assert_eq!(rebuilt.executions_completed, expected_executions);
        assert!(rebuilt.executions_completed < 32);
        assert!(!checkpoint.entries.is_empty());
        assert!(checkpoint.entries.len() <= rebuilt.archive.entries.len());
        let report_ids = rebuilt
            .archive
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>();
        assert!(
            checkpoint
                .entries
                .iter()
                .all(|snapshot| report_ids.contains(&snapshot.id))
        );
    }

    #[test]
    fn unprobed_campaign_probes_nothing_and_replays_byte_identically() {
        let rom = synthetic_nrom();
        let probing = genesis_config(0x5eed_ca20, 4, 32);
        let mut probing_stream = Vec::new();
        let probed = run_smb_campaign(
            &rom,
            &probing,
            &SmbCampaignOrigin::Genesis,
            &mut probing_stream,
        )
        .expect("probing campaign");
        let mut config = genesis_config(0x5eed_ca20, 4, 32);
        config.retention = crate::search::archive::RetentionPolicy::Unprobed;
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("admit-alive campaign");
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        let header = text.lines().next().expect("header");
        assert!(header.contains("\"retention_policy\":\"unprobed\""));
        assert_eq!(live.probe_refused, 0);
        assert_eq!(
            live.execution_work, probed.execution_work,
            "probe work is restored and excluded from logical execution work"
        );
        let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay admit-alive");
        assert_eq!(
            serde_json::to_vec_pretty(&live).expect("serialize live"),
            serde_json::to_vec_pretty(&replayed).expect("serialize replayed")
        );
    }

    #[test]
    fn unprobed_reports_survive_a_seed_sweep() {
        let rom = synthetic_nrom();
        for seed in 0..24_u64 {
            let mut config = genesis_config(0x5eed_d000 + seed, 4, 64);
            config.retention = crate::search::archive::RetentionPolicy::Unprobed;
            let mut stream = Vec::new();
            let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
                .expect("reset-heavy campaign");
            let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay reset-heavy");
            assert_eq!(live, replayed, "seed offset {seed} diverged");
        }
    }

    #[test]
    fn retention_identifiers_round_trip() {
        use crate::search::archive::{
            RetentionPolicy, retention_policy_from_identifier, retention_policy_identifier,
        };
        for policy in [RetentionPolicy::ProbeAtAdmission, RetentionPolicy::Unprobed] {
            assert_eq!(
                retention_policy_from_identifier(retention_policy_identifier(policy))
                    .expect("retention round trip"),
                policy
            );
        }
        assert!(retention_policy_from_identifier("no_probe").is_err());
    }

    #[test]
    fn a_stream_recorded_under_another_rule_is_refused() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca10, 1, 4);
        let mut stream = Vec::new();
        run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign");
        let text = String::from_utf8(stream).expect("stream is utf-8");
        for (from, to) in [
            (
                crate::search::archive::SELECTOR_IDENTIFIER,
                "concentrated_recency_128",
            ),
            ("probe_at_admission", "probe_at_admission_snapback_16"),
            ("fewest_frames_in_level", "fewest_actions"),
            ("\"whole_tree\"", "\"frontier_shortest\""),
            ("nes_pressable_36", "frozen_nine_mask"),
            ("planned_finish_window_1_v6", "unknown_order_v9"),
        ] {
            let tampered = text.replacen(from, to, 1);
            assert!(
                replay_smb_campaign(&rom, tampered.as_bytes(), None).is_err(),
                "replay accepted {to}"
            );
        }
    }

    #[test]
    fn a_folded_draw_table_records_advancing_checkpoints_and_replays() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca13, 1, 20);
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("folded draw table campaign");
        let text = std::str::from_utf8(&stream).expect("stream text");
        assert!(
            text.lines()
                .next()
                .expect("header")
                .contains("\"draw_header\"")
        );
        assert!(text.contains("\"draw_checkpoint_before\""));
        let checkpoints = text
            .lines()
            .skip(1)
            .map(|line| {
                serde_json::from_str::<SmbCampaignStreamRecord>(line)
                    .expect("parse campaign record")
            })
            .filter_map(|record| match record {
                SmbCampaignStreamRecord::Job(job) => job.draw_checkpoint_before,
                SmbCampaignStreamRecord::Skip(skip) => skip.draw_checkpoint_before,
                SmbCampaignStreamRecord::End(_) => None,
            })
            .collect::<Vec<_>>();
        assert!(
            checkpoints.windows(2).all(|pair| {
                pair[0].records <= pair[1].records
                    && pair[0].retained_successes <= pair[1].retained_successes
            }),
            "recorded table checkpoints must advance monotonically"
        );
        assert!(
            checkpoints.len() > 1,
            "every draw must record the table it read"
        );
        let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay folded draw table");
        assert_eq!(live, replayed);
    }

    #[test]
    fn duplicate_check_requires_every_boundary() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca04, 2, 16);
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign");
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        if let Some(skip_line) = text.lines().find(|line| line.contains("\"skip\"")) {
            let tampered_line = skip_line.replace("\"mutation_seed\":", "\"mutation_seed\":9");
            let tampered = text.replace(skip_line, &tampered_line);
            let outcome = replay_smb_campaign(&rom, tampered.as_bytes(), None);
            assert!(outcome.is_err());
        }
        assert_eq!(
            live.duplicates_skipped,
            text.lines()
                .filter(|line| line.contains("\"skip\""))
                .count() as u64
        );
    }

    #[test]
    fn archive_origin_round_trips_through_replay() {
        use super::{SmbCampaignCheckpoint, SmbSnapshotCheckpoint};
        use sha2::{Digest, Sha256};
        let rom = synthetic_nrom();
        let seed_config = genesis_config(0x5eed_ca05, 2, 12);
        let mut seed_stream = Vec::new();
        let (seed_campaign, seed_checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &seed_config,
            &SmbCampaignOrigin::Genesis,
            &mut seed_stream,
            None,
        )
        .expect("seed campaign");
        let source = seed_campaign.archive.clone();
        for saved in &seed_checkpoint.entries {
            let identity = saved
                .snapshot
                .snapshot_identity_sha256()
                .expect("snapshot identity");
            let entry = source
                .entries
                .iter()
                .find(|entry| entry.id == saved.id)
                .expect("checkpoint snapshot has a source entry");
            assert_eq!(
                entry.snapshot_identity_sha256.as_deref(),
                Some(identity.as_str())
            );
        }
        let archive_source = ArchiveSource::from_bytes(&serde_json::to_vec(&source).unwrap())
            .expect("source archive bytes parse");
        let tree_config = genesis_config(0x5eed_ca06, 1, 16);
        let mut tree_stream = Vec::new();
        let tree_live = run_smb_campaign(
            &rom,
            &tree_config,
            &SmbCampaignOrigin::Archive {
                path: "seed-archive.json".to_owned(),
                source: archive_source.clone(),
                checkpoint: None,
            },
            &mut tree_stream,
        )
        .expect("whole-tree campaign");
        let tree_replayed = replay_smb_campaign(&rom, &tree_stream, Some(&archive_source))
            .expect("replay whole-tree campaign");
        assert_eq!(tree_live, tree_replayed);
        assert_eq!(tree_live.origin.kind, "archive");
        assert_eq!(tree_live.resume_policy, "whole_tree");
        let counts = tree_live.tree_import.expect("tree import counts");
        let source_retained = u64::try_from(source.entries.len() - 1).expect("count");
        assert_eq!(
            counts.imported + counts.duplicate + counts.rejected + counts.terminal,
            source_retained
        );
        assert!(counts.imported >= 1);
        let source_ids = source
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>();
        let sparse_parents = source
            .entries
            .iter()
            .filter(|entry| {
                entry
                    .parent_id
                    .is_some_and(|parent| !source_ids.contains(&parent))
            })
            .count();
        assert_eq!(
            counts.rerooted,
            u64::try_from(sparse_parents).expect("sparse parent count")
        );
        assert_eq!(counts.checkpointed, 0);

        let checkpoint_bytes = seed_checkpoint.to_bytes().expect("encode checkpoint");
        let checkpoint = SmbCampaignCheckpoint {
            path: "seed-snapshots.bin".to_owned(),
            file_sha256: format!("{:x}", Sha256::digest(&checkpoint_bytes)),
            snapshots: SmbSnapshotCheckpoint::from_bytes(
                &checkpoint_bytes,
                super::SNAPSHOT_CHECKPOINT_FORMAT,
            )
            .expect("decode checkpoint"),
        };
        assert_eq!(checkpoint.snapshots.entries.len(), source.entries.len());
        let checkpointed_source_entries = u64::try_from(
            checkpoint
                .snapshots
                .entries
                .iter()
                .filter(|entry| entry.id != 0)
                .count(),
        )
        .expect("checkpoint entry count fits u64");
        let mut restored_stream = Vec::new();
        let (restored_live, restored_checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &tree_config,
            &SmbCampaignOrigin::Archive {
                path: "seed-archive.json".to_owned(),
                source: archive_source.clone(),
                checkpoint: Some(checkpoint.clone()),
            },
            &mut restored_stream,
            None,
        )
        .expect("checkpoint-restored campaign");
        assert_eq!(restored_live.archive, tree_live.archive);
        assert!(restored_live.bootstrap_execution_work < tree_live.bootstrap_execution_work);
        assert_eq!(
            restored_live.origin.checkpoint_sha256.as_deref(),
            Some(checkpoint.file_sha256.as_str())
        );
        let restored_counts = restored_live.tree_import.expect("restored counts");
        assert_eq!(restored_counts.checkpointed, checkpointed_source_entries);
        assert_eq!(
            (
                restored_counts.imported,
                restored_counts.duplicate,
                restored_counts.rejected
            ),
            (counts.imported, counts.duplicate, counts.rejected)
        );
        let (replayed_with, replayed_checkpoint) = replay_smb_campaign_checkpointed(
            &rom,
            &restored_stream,
            Some(&archive_source),
            Some(&checkpoint),
        )
        .expect("replay with checkpoint");
        assert_eq!(replayed_with, restored_live);
        assert_eq!(replayed_checkpoint, restored_checkpoint);
        let moved_checkpoint = SmbCampaignCheckpoint {
            path: "moved/seed-snapshots.bin".to_owned(),
            ..checkpoint.clone()
        };
        assert!(
            replay_smb_campaign_checkpointed(
                &rom,
                &restored_stream,
                Some(&archive_source),
                Some(&moved_checkpoint),
            )
            .is_err()
        );
        assert!(
            replay_smb_campaign_checkpointed(&rom, &restored_stream, Some(&archive_source), None)
                .is_err()
        );
        let wrong = SmbCampaignCheckpoint {
            file_sha256: "00".to_owned(),
            ..checkpoint.clone()
        };
        assert!(
            replay_smb_campaign_checkpointed(
                &rom,
                &restored_stream,
                Some(&archive_source),
                Some(&wrong)
            )
            .is_err()
        );
        let imported_inputs: std::collections::BTreeSet<_> = tree_live
            .archive
            .entries
            .iter()
            .filter(|entry| entry.created_execution == 0)
            .map(|entry| entry.input.clone())
            .collect();
        let source_inputs: std::collections::BTreeSet<_> = source
            .entries
            .iter()
            .map(|entry| entry.input.clone())
            .collect();
        assert!(imported_inputs.is_subset(&source_inputs));
        assert!(!imported_inputs.is_empty());
    }

    #[test]
    fn the_progress_sidecar_changes_no_recorded_bytes() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca0e, 1, 24);
        let mut without = Vec::new();
        run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut without)
            .expect("campaign without a sidecar");
        let mut with = Vec::new();
        let mut sidecar = Vec::new();
        let observed = run_smb_campaign_with_progress(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut with,
            Some(&mut sidecar),
        )
        .expect("campaign with a sidecar");
        assert_eq!(without, with);
        assert!(!with.is_empty());
        assert!(
            std::str::from_utf8(&with)
                .expect("stream is utf-8")
                .lines()
                .all(|line| !line.contains("unix_time")),
            "no sidecar field reaches the recorded stream"
        );
        let records: Vec<SmbCampaignProgressRecord> = std::str::from_utf8(&sidecar)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.first().unwrap().executions, 1);
        let progress = records.last().unwrap();
        assert_eq!(progress.executions, observed.executions_completed);
        assert_eq!(progress.execution_work, observed.execution_work);
        assert!(progress.progress.is_some());
        assert!(progress.search_elapsed_millis.is_some());
        let replayed =
            replay_smb_campaign(&rom, &with, None).expect("sidecar run replays byte-exact");
        assert_eq!(replayed.stream_sha256, observed.stream_sha256);
        assert_eq!(replayed.archive, observed.archive);
    }
}

#[cfg(test)]
mod evidence_checkpoint_tests {
    use super::*;
    use crate::{search::campaign::Reporting, smb::target::ButtonChord};
    #[test]
    fn evidence_round_trip_preserves_all_serialized_fields_and_rejects_truncation() {
        let mut evidence = SmbCampaignEvidence::default();
        evidence.aggregate.reached_1_2 = true;
        evidence.first_reached.level_1_2 = Some(23);
        evidence.champion_input.actions.push(ButtonChord {
            buttons: 1,
            hold_frames: 2,
        });
        evidence.champion_milestones = evidence.aggregate;
        let bytes = <SmbGame as Reporting>::evidence_checkpoint(&evidence).unwrap();
        let restored = <SmbGame as Reporting>::evidence_from_checkpoint(&bytes).unwrap();
        assert_eq!(
            serde_json::to_value(&evidence).unwrap(),
            serde_json::to_value(&restored).unwrap()
        );
        assert!(
            <SmbGame as Reporting>::evidence_from_checkpoint(&bytes[..bytes.len() - 1]).is_err()
        );
    }
}
