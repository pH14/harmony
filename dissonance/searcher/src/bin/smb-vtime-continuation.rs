// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sample real campaign branch points and compare restored continuation hashes.

#[cfg(unix)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{env, fs, path::PathBuf};

    use searcher::{
        search::campaign::SnapshotCheckpoint,
        smb::{
            campaign::{REMOTE_SNAPSHOT_CHECKPOINT_FORMAT, RemoteSmbSnapshotCheckpoint},
            remote::{ContinuationHashEvidence, DifferentialSmbTarget},
            target::{ButtonChord, SmbCampaignTarget},
        },
        target::Target,
    };
    use serde::Serialize;

    #[derive(Serialize)]
    struct BranchSample {
        archive_id: u64,
        lineage_actions: usize,
        branch_state_hash: String,
        chord_state_hashes: Vec<String>,
    }

    #[derive(Serialize)]
    struct ContinuationReport {
        format: &'static str,
        sampled_branch_points: usize,
        chord_hashes_compared: usize,
        samples: Vec<BranchSample>,
    }

    fn hex(hash: [u8; 32]) -> String {
        hash.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn sample_actions(id: u64) -> Result<[ButtonChord; 2], Box<dyn std::error::Error>> {
        let masks = searcher::smb::campaign::SmbButtonVocabulary::NesDownTen.masks();
        let mask_count = u64::try_from(masks.len())?;
        let first = usize::try_from(id % mask_count)?;
        let second = usize::try_from((id / mask_count + 3) % mask_count)?;
        let first_hold = u8::try_from(2 + (id % 11))?;
        let second_hold = u8::try_from(2 + ((id / 7) % 11))?;
        Ok([
            ButtonChord::new(masks[first], first_hold),
            ButtonChord::new(masks[second], second_hold),
        ])
    }

    let mut args = env::args_os().skip(1);
    let socket = PathBuf::from(
        args.next()
            .ok_or("usage: smb-vtime-continuation <socket> <checkpoint> <samples> <report>")?,
    );
    let checkpoint_path = PathBuf::from(args.next().ok_or("missing checkpoint path")?);
    let sample_count = args
        .next()
        .ok_or("missing sample count")?
        .to_string_lossy()
        .parse::<usize>()?;
    let report_path = PathBuf::from(args.next().ok_or("missing report path")?);
    if sample_count == 0 {
        return Err("sample count must be positive".into());
    }
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(rom_path)?;
    let checkpoint: RemoteSmbSnapshotCheckpoint = SnapshotCheckpoint::from_bytes(
        &fs::read(checkpoint_path)?,
        REMOTE_SNAPSHOT_CHECKPOINT_FORMAT,
    )?;
    let eligible = checkpoint
        .entries
        .iter()
        .filter(|entry| entry.snapshot.lineage_len() > 0)
        .collect::<Vec<_>>();
    if eligible.len() < sample_count {
        return Err(format!(
            "checkpoint has only {} mid-workload branch points, need {sample_count}",
            eligible.len()
        )
        .into());
    }

    let mut target = DifferentialSmbTarget::connect(socket, &rom)?;
    let mut samples = Vec::with_capacity(sample_count);
    let mut chord_hashes_compared = 0_usize;
    for sample_index in 0..sample_count {
        let entry = eligible[sample_index * eligible.len() / sample_count];
        target.restore(&entry.snapshot)?;
        if target.campaign_diverged() {
            return Err(format!(
                "cross-build differential diverged at archive entry {}",
                entry.id
            )
            .into());
        }
        let ContinuationHashEvidence {
            branch_state_hash,
            chord_state_hashes,
        } = target.verify_current_continuation(&sample_actions(entry.id)?)?;
        if chord_state_hashes.is_empty() {
            return Err(format!(
                "archive entry {} produced no continuation chord hash",
                entry.id
            )
            .into());
        }
        chord_hashes_compared = chord_hashes_compared.saturating_add(chord_state_hashes.len());
        samples.push(BranchSample {
            archive_id: entry.id,
            lineage_actions: entry.snapshot.lineage_len(),
            branch_state_hash: hex(branch_state_hash),
            chord_state_hashes: chord_state_hashes.into_iter().map(hex).collect(),
        });
    }

    let report = ContinuationReport {
        format: "smb-vtime-continuation-v1",
        sampled_branch_points: samples.len(),
        chord_hashes_compared,
        samples,
    };
    fs::write(&report_path, serde_json::to_vec_pretty(&report)?)?;
    println!(
        "M2_CONTINUATION_ORACLE_OK samples={} chord_hashes={chord_hashes_compared}",
        report.sampled_branch_points
    );
    Ok(())
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("smb-vtime-continuation requires a Unix control socket");
    std::process::ExitCode::from(2)
}
