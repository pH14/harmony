// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic champion–challenger campaigns for the SMB completion experiment.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fs,
    path::PathBuf,
};

use fuzzer::{
    phase2::{Flag, Interest, TriageLabels},
    phase4b::{
        NullSmbDetector, NullSmbMacro, SmbArtifactConfig, SmbCampaignReport, SmbConfiguredReport,
        SmbInput, SmbLabeledCorpusEntry, SmbMilestones, observe_smb_input,
        run_smb_restart_configured, smb_milestones_from_wram,
    },
    phase4c::{
        MAX_SMB_COMPLETION_ACTIONS, SmbArchiveDurationPolicy, SmbArchiveReport,
        SmbArchiveSuffixPolicy, run_smb_archive_search,
        run_smb_archive_search_with_config_and_suffix, run_smb_archive_search_with_policies,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const M12_PILOT_SEED: u64 = 0x5eed_dc00;
const M12_PILOT_EXECUTIONS: u64 = 500;
const FRONTIER_RESUME_INPUTS: usize = 64;

#[derive(Clone, Copy)]
enum ResumeSelection {
    Champion,
    Frontier,
    FrontierSet,
    FrontierBandSet,
}

#[derive(Debug, Deserialize)]
struct M5Report {
    ratchet: Vec<SmbCampaignReport>,
}

#[derive(Debug, Serialize)]
struct BaselineReproduction {
    base_commit: &'static str,
    rom_sha256: String,
    source_seed: u64,
    source_executions: u64,
    source_max_x: u16,
    source_corpus_count: usize,
    pilot_seed: u64,
    pilot_executions: u64,
    pilot_max_x: u16,
    pilot_corpus_count: usize,
    no_model_campaign_replay_verified: bool,
    champion_milestones: SmbMilestones,
    champion_input_sha256: String,
    champion_observations_sha256: String,
    champion_observation_replay_verified: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let mode = args
        .next()
        .ok_or("usage: smb-completion <reproduce-baseline|control|archive> ...")?;
    if mode == "control" {
        return run_control_mode(&mut args);
    }
    if mode == "archive" {
        return run_archive_mode(&mut args);
    }
    if mode == "archive-resume" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Legacy,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::Champion,
        );
    }
    if mode == "archive-resume-temporal" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::Champion,
        );
    }
    if mode == "archive-resume-frontier" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::Frontier,
        );
    }
    if mode == "archive-resume-frontier-burst" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::BurstUpToFour,
            ResumeSelection::Frontier,
        );
    }
    if mode == "archive-resume-frontier-set" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::OneOrTwo,
            ResumeSelection::FrontierSet,
        );
    }
    if mode == "archive-resume-burst" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::BurstUpToFour,
            ResumeSelection::FrontierSet,
        );
    }
    if mode == "archive-resume-band-burst" {
        return run_archive_resume_mode(
            &mut args,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::BurstUpToFour,
            ResumeSelection::FrontierBandSet,
        );
    }
    if mode != "reproduce-baseline" {
        return Err("unknown smb-completion mode".into());
    }
    let source_path = PathBuf::from(args.next().ok_or("missing M5 report")?);
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    fs::create_dir_all(&output)?;

    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(rom_path)?;
    let source: M5Report = serde_json::from_slice(&fs::read(source_path)?)?;
    let source_run = source
        .ratchet
        .iter()
        .max_by_key(|run| milestone_key(run.milestones))
        .ok_or("M5 report contains no ratchet runs")?;
    let initial_corpus = source_run
        .corpus
        .iter()
        .cloned()
        .map(|input| SmbLabeledCorpusEntry {
            input,
            labels: neutral_labels(),
        })
        .collect::<Vec<_>>();
    fs::write(
        output.join("initial-corpus.json"),
        serde_json::to_vec_pretty(&initial_corpus)?,
    )?;

    let first = run_control(&rom, &initial_corpus)?;
    let replay = run_control(&rom, &initial_corpus)?;
    let no_model_campaign_replay_verified = first == replay;
    fs::write(
        output.join("pilot-live.json"),
        serde_json::to_vec_pretty(&first)?,
    )?;
    fs::write(
        output.join("pilot-replay.json"),
        serde_json::to_vec_pretty(&replay)?,
    )?;

    let (champion, champion_milestones) = first
        .campaign
        .corpus
        .iter()
        .map(|input| {
            let milestones = input_milestones(&rom, input)?;
            Ok::<_, Box<dyn Error>>((input, milestones))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max_by_key(|(_, milestones)| milestone_key(*milestones))
        .ok_or("M12 pilot retained no inputs")?;
    let observations = observe_smb_input(&rom, champion)?;
    let replayed_observations = observe_smb_input(&rom, champion)?;
    let champion_observation_replay_verified = observations == replayed_observations;
    let input_bytes = serde_json::to_vec(champion)?;
    let observation_bytes = serde_json::to_vec(&observations)?;
    let champion_input_sha256 = format!("{:x}", Sha256::digest(&input_bytes));
    let champion_observations_sha256 = format!("{:x}", Sha256::digest(&observation_bytes));
    fs::write(
        output.join("starting-champion-input.json"),
        serde_json::to_vec_pretty(champion)?,
    )?;
    fs::write(
        output.join("starting-champion-observations.json"),
        serde_json::to_vec_pretty(&observations)?,
    )?;

    let report = BaselineReproduction {
        base_commit: "8f2b522c26c6f192f2db45a430bec03ed447cad7",
        rom_sha256: format!("{:x}", Sha256::digest(&rom)),
        source_seed: source_run.seed,
        source_executions: source_run.executions,
        source_max_x: source_run.milestones.max_1_1_scroll_bucket,
        source_corpus_count: source_run.corpus.len(),
        pilot_seed: M12_PILOT_SEED,
        pilot_executions: M12_PILOT_EXECUTIONS,
        pilot_max_x: first.campaign.milestones.max_1_1_scroll_bucket,
        pilot_corpus_count: first.campaign.corpus.len(),
        no_model_campaign_replay_verified,
        champion_milestones,
        champion_input_sha256,
        champion_observations_sha256,
        champion_observation_replay_verified,
    };
    fs::write(
        output.join("baseline-reproduction.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.no_model_campaign_replay_verified || !report.champion_observation_replay_verified {
        return Err("frozen baseline replay diverged".into());
    }
    Ok(())
}

fn run_control_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source = PathBuf::from(args.next().ok_or("missing baseline pilot report")?);
    let seed = parse_u64(&args.next().ok_or("missing seed")?.to_string_lossy())?;
    let budget = parse_u64(
        &args
            .next()
            .ok_or("missing execution budget")?
            .to_string_lossy(),
    )?;
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "control")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbConfiguredReport = serde_json::from_slice(&fs::read(source)?)?;
    let corpus = neutral_corpus(&source.campaign.corpus);
    let report = run_smb_restart_configured(
        &rom,
        &corpus,
        seed,
        budget,
        NullSmbDetector,
        NullSmbMacro,
        no_artifacts(),
    )?;
    fs::write(
        output.join("control-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let replay_verified = if replay_requested {
        let replay = run_smb_restart_configured(
            &rom,
            &corpus,
            seed,
            budget,
            NullSmbDetector,
            NullSmbMacro,
            no_artifacts(),
        )?;
        fs::write(
            output.join("control-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay == report)
    } else {
        None
    };
    let summary = serde_json::json!({
        "seed": seed,
        "executions": budget,
        "milestones": report.campaign.milestones,
        "corpus_count": report.campaign.corpus.len(),
        "replay_verified": replay_verified,
    });
    fs::write(
        output.join("control-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn run_archive_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
) -> Result<(), Box<dyn Error>> {
    let source = PathBuf::from(args.next().ok_or("missing baseline pilot report")?);
    let seed = parse_u64(&args.next().ok_or("missing seed")?.to_string_lossy())?;
    let budget = parse_u64(
        &args
            .next()
            .ok_or("missing execution budget")?
            .to_string_lossy(),
    )?;
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "archive")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbConfiguredReport = serde_json::from_slice(&fs::read(source)?)?;
    let report = run_smb_archive_search(&rom, &source.campaign.corpus, seed, budget)?;
    fs::write(
        output.join("archive-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let replay_verified = if replay_requested {
        let replay = run_smb_archive_search(&rom, &source.campaign.corpus, seed, budget)?;
        fs::write(
            output.join("archive-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay == report)
    } else {
        None
    };
    let observations = observe_smb_input(&rom, &report.champion_input)?;
    let champion_input_sha256 = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&report.champion_input)?)
    );
    let champion_observations_sha256 =
        format!("{:x}", Sha256::digest(serde_json::to_vec(&observations)?));
    let summary = archive_summary(
        &report,
        replay_verified,
        champion_input_sha256,
        champion_observations_sha256,
    );
    fs::write(
        output.join("archive-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn run_archive_resume_mode(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    duration_policy: SmbArchiveDurationPolicy,
    suffix_policy: SmbArchiveSuffixPolicy,
    selection: ResumeSelection,
) -> Result<(), Box<dyn Error>> {
    let source = PathBuf::from(args.next().ok_or("missing source archive report")?);
    let seed = parse_u64(&args.next().ok_or("missing seed")?.to_string_lossy())?;
    let budget = parse_u64(
        &args
            .next()
            .ok_or("missing execution budget")?
            .to_string_lossy(),
    )?;
    let action_limit_u64 = parse_u64(
        &args
            .next()
            .ok_or("missing completion action limit")?
            .to_string_lossy(),
    )?;
    let action_limit = usize::try_from(action_limit_u64)?;
    if action_limit > MAX_SMB_COMPLETION_ACTIONS {
        return Err("completion action limit exceeds the compiled bound".into());
    }
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let replay_requested = parse_replay_flag(args, "archive-resume")?;
    fs::create_dir_all(&output)?;
    let rom = read_rom()?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(source)?)?;
    let initial = match selection {
        ResumeSelection::Champion => vec![source.champion_input],
        ResumeSelection::Frontier => vec![
            frontier_entries(&source)?
                .first()
                .ok_or("source archive contains no frontier entries")?
                .input
                .clone(),
        ],
        ResumeSelection::FrontierSet => {
            let mut distinct = BTreeMap::new();
            for entry in frontier_entries(&source)? {
                distinct
                    .entry((entry.key.player_y_bucket, entry.key.player_engine_state))
                    .or_insert_with(|| entry.input.clone());
            }
            distinct
                .into_values()
                .take(FRONTIER_RESUME_INPUTS)
                .collect()
        }
        ResumeSelection::FrontierBandSet => {
            let mut distinct = BTreeSet::new();
            let mut by_progress = BTreeMap::<u16, Vec<_>>::new();
            for entry in frontier_band_entries(&source)? {
                by_progress
                    .entry(entry.key.progress)
                    .or_default()
                    .push(entry);
            }
            by_progress
                .into_values()
                .flat_map(|entries| entries.into_iter().take(8))
                .filter_map(|entry| {
                    distinct
                        .insert(entry.input.clone())
                        .then_some(entry.input.clone())
                })
                .take(FRONTIER_RESUME_INPUTS)
                .collect()
        }
    };
    let frozen_search = matches!(
        selection,
        ResumeSelection::Champion | ResumeSelection::Frontier
    );
    let report = if frozen_search {
        run_smb_archive_search_with_config_and_suffix(
            &rom,
            &initial,
            seed,
            budget,
            action_limit,
            duration_policy,
            suffix_policy,
        )?
    } else {
        run_smb_archive_search_with_policies(
            &rom,
            &initial,
            seed,
            budget,
            action_limit,
            duration_policy,
            suffix_policy,
        )?
    };
    fs::write(
        output.join("archive-live.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    let replay_verified = if replay_requested {
        let replay = if frozen_search {
            run_smb_archive_search_with_config_and_suffix(
                &rom,
                &initial,
                seed,
                budget,
                action_limit,
                duration_policy,
                suffix_policy,
            )?
        } else {
            run_smb_archive_search_with_policies(
                &rom,
                &initial,
                seed,
                budget,
                action_limit,
                duration_policy,
                suffix_policy,
            )?
        };
        fs::write(
            output.join("archive-replay.json"),
            serde_json::to_vec_pretty(&replay)?,
        )?;
        Some(replay == report)
    } else {
        None
    };
    let observations = observe_smb_input(&rom, &report.champion_input)?;
    let summary = archive_summary(
        &report,
        replay_verified,
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&report.champion_input)?)
        ),
        format!("{:x}", Sha256::digest(serde_json::to_vec(&observations)?)),
    );
    let summary = serde_json::json!({
        "action_limit": action_limit,
        "source_selection": match selection {
            ResumeSelection::Champion => "champion",
            ResumeSelection::Frontier => "mechanical_frontier",
            ResumeSelection::FrontierSet => "mechanical_frontier_set",
            ResumeSelection::FrontierBandSet => "mechanical_frontier_band_set",
        },
        "source_inputs": initial.len(),
        "suffix_policy": match suffix_policy {
            SmbArchiveSuffixPolicy::OneOrTwo => "one_or_two",
            SmbArchiveSuffixPolicy::BurstUpToFour => "burst_up_to_four",
        },
        "campaign": summary,
    });
    fs::write(
        output.join("archive-summary.json"),
        serde_json::to_vec_pretty(&summary)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn frontier_entries(
    source: &SmbArchiveReport,
) -> Result<Vec<&fuzzer::phase4c::SmbArchiveEntryReport>, Box<dyn Error>> {
    let frontier = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
        .max()
        .ok_or("source archive contains no retained entries")?;
    let mut entries = source
        .entries
        .iter()
        .filter(|entry| (entry.key.world, entry.key.level, entry.key.progress) == frontier)
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (entry.input.actions.len(), entry.id));
    Ok(entries)
}

fn frontier_band_entries(
    source: &SmbArchiveReport,
) -> Result<Vec<&fuzzer::phase4c::SmbArchiveEntryReport>, Box<dyn Error>> {
    let frontier = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
        .max()
        .ok_or("source archive contains no retained entries")?;
    let mut entries = source
        .entries
        .iter()
        .filter(|entry| {
            entry.key.world == frontier.0
                && entry.key.level == frontier.1
                && entry.key.progress.saturating_add(7) >= frontier.2
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| (entry.input.actions.len(), entry.id));
    Ok(entries)
}

fn archive_summary(
    report: &SmbArchiveReport,
    replay_verified: Option<bool>,
    champion_input_sha256: String,
    champion_observations_sha256: String,
) -> serde_json::Value {
    serde_json::json!({
        "seed": report.seed,
        "executions": report.executions,
        "milestones": report.milestones,
        "entries": report.entries.len(),
        "retained": report.retained,
        "rejected": report.rejected,
        "deaths": report.deaths,
        "replay_verified": replay_verified,
        "champion_input_sha256": champion_input_sha256,
        "champion_observations_sha256": champion_observations_sha256,
    })
}

fn read_rom() -> Result<Vec<u8>, Box<dyn Error>> {
    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    Ok(fs::read(rom_path)?)
}

fn parse_u64(value: &str) -> Result<u64, Box<dyn Error>> {
    let normalized = value.replace('_', "");
    if let Some(hex) = normalized.strip_prefix("0x") {
        Ok(u64::from_str_radix(hex, 16)?)
    } else {
        Ok(normalized.parse()?)
    }
}

fn parse_replay_flag(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    mode: &str,
) -> Result<bool, Box<dyn Error>> {
    let replay = match args.next() {
        None => false,
        Some(value) if value == "--replay" => true,
        Some(_) => return Err(format!("unexpected {mode} argument").into()),
    };
    if args.next().is_some() {
        return Err(format!("unexpected extra {mode} argument").into());
    }
    Ok(replay)
}

fn neutral_corpus(inputs: &[SmbInput]) -> Vec<SmbLabeledCorpusEntry> {
    inputs
        .iter()
        .cloned()
        .map(|input| SmbLabeledCorpusEntry {
            input,
            labels: neutral_labels(),
        })
        .collect()
}

fn no_artifacts() -> SmbArtifactConfig<'static> {
    SmbArtifactConfig {
        detector_name: "none",
        detector_retire_after: u64::MAX,
        macro_name: "none",
        macro_retire_after: u64::MAX,
        enable_macro: false,
    }
}

fn run_control(
    rom: &[u8],
    initial_corpus: &[SmbLabeledCorpusEntry],
) -> Result<SmbConfiguredReport, Box<dyn Error>> {
    run_smb_restart_configured(
        rom,
        initial_corpus,
        M12_PILOT_SEED,
        M12_PILOT_EXECUTIONS,
        NullSmbDetector,
        NullSmbMacro,
        SmbArtifactConfig {
            detector_name: "none",
            detector_retire_after: u64::MAX,
            macro_name: "none",
            macro_retire_after: u64::MAX,
            enable_macro: false,
        },
    )
}

fn input_milestones(rom: &[u8], input: &SmbInput) -> Result<SmbMilestones, Box<dyn Error>> {
    let mut aggregate = SmbMilestones::default();
    for observation in observe_smb_input(rom, input)? {
        let wram: &[u8; 2_048] = observation
            .wram
            .as_slice()
            .try_into()
            .map_err(|_| "SMB observation WRAM is not exactly 2 KiB")?;
        let current = smb_milestones_from_wram(wram);
        aggregate.max_1_1_scroll_bucket = aggregate
            .max_1_1_scroll_bucket
            .max(current.max_1_1_scroll_bucket);
        aggregate.reached_1_1_flag |= current.reached_1_1_flag;
        aggregate.reached_1_2 |= current.reached_1_2;
        aggregate.reached_onward |= current.reached_onward;
    }
    Ok(aggregate)
}

fn milestone_key(milestones: SmbMilestones) -> (bool, bool, bool, u16) {
    (
        milestones.reached_onward,
        milestones.reached_1_2,
        milestones.reached_1_1_flag,
        milestones.max_1_1_scroll_bucket,
    )
}

fn neutral_labels() -> TriageLabels {
    TriageLabels {
        interest: Interest::Neutral,
        duplicate_of: None,
        flags: Vec::<Flag>::new(),
        tags: Vec::new(),
        summary: "neutral frozen-baseline label".to_owned(),
        hypotheses: Vec::new(),
    }
}
