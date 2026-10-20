// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    fs,
    num::NonZeroU64,
    path::{Path, PathBuf},
};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    famidash::{
        campaign::{FamidashCampaignRun, FamidashGame},
        levels::{Side, Stage},
    },
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
    twin_dragons::campaign::{TwinDragonsCampaignRun, TwinDragonsGame},
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
                    checkpoint_owner: None,
                    max_image_bytes: 64 << 20,
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
        CampaignOrigin::SearchCheckpoint {
            path: interval,
            expected_sha256: None,
        },
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
    assert_resume_restores_evidence(&SmbGame::loopback_for_tests(&[0]), smb_run, "smb-loopback");
    let roms = [
        "HARMONY_SMB_ROM",
        "HARMONY_MM2_ROM",
        "HARMONY_METROID_ROM",
        "HARMONY_STB_ROM",
        "HARMONY_FAMIDASH_ROM",
        "HARMONY_TWIN_DRAGONS_ROM",
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
    let execution_sha256 =
        machine::quicknes::prepared_execution_sha256(&core, &core_sha256).unwrap();
    let [smb, mm2, metroid, stb, famidash, twin_dragons] = roms;
    if let Some(rom) = smb {
        let game = SmbGame::new(&rom, &core, &core_sha256, &execution_sha256);
        assert_resume_restores_evidence(&game, smb_run, "smb");
    }
    if let Some(rom) = mm2 {
        let game =
            Mm2Game::new_whole_game(&rom, &core, &core_sha256, &execution_sha256, Vec::new());
        assert_resume_restores_evidence(&game, Mm2CampaignRun, "mm2");
    }
    if let Some(rom) = metroid {
        let game = MetroidGame::new(&rom, &core, &core_sha256, &execution_sha256);
        assert_resume_restores_evidence(&game, MetroidCampaignRun, "metroid");
    }
    if let Some(rom) = stb {
        let game = StbGame::with_ai(&rom, &core, &core_sha256, &execution_sha256, StbAi::Hard);
        assert_resume_restores_evidence(&game, StbCampaignRun, "stb");
    }
    if let Some(rom) = famidash {
        let stage = Stage::single(Side::Main, 0).unwrap();
        let game = FamidashGame::new(&rom, &core, &core_sha256, &execution_sha256, stage);
        assert_resume_restores_evidence(&game, FamidashCampaignRun, "famidash");
    }
    if let Some(rom) = twin_dragons {
        let game = TwinDragonsGame::new(&rom, &core, &core_sha256, &execution_sha256);
        assert_resume_restores_evidence(&game, TwinDragonsCampaignRun, "twin-dragons");
    }
}
