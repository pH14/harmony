// SPDX-License-Identifier: AGPL-3.0-or-later

//! M6 live-model orchestration, generated-artifact validation, restart, and replay.

use std::{
    cmp::Reverse,
    collections::BTreeSet,
    env,
    error::Error,
    ffi::OsStr,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use fuzzer::{
    phase2::{Flag, Interest, TriageLabels},
    phase4a::{InstrumentorAction, InstrumentorDecision, InstrumentorRequest},
    phase4b::{
        NullSmbDetector, NullSmbMacro, SmbArtifactConfig, SmbCampaignReport, SmbConfiguredReport,
        SmbLabeledCorpusEntry, SmbTriageRequest, observe_smb_input, run_smb_restart_configured,
    },
    phase4c::SmbArchiveReport,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

const SEEDS: [u64; 6] = [
    0x5eed_d800,
    0x5eed_d801,
    0x5eed_d802,
    0x5eed_d803,
    0x5eed_d804,
    0x5eed_d805,
];
const EXECUTION_BUDGET: u64 = 500;
const MAX_TRIAGE_CALLS: usize = 200;
const MAX_ATTEMPTS: u8 = 3;
const SOURCE_LIMIT: usize = 262_144;
const ERROR_LIMIT: usize = 16_384;
const PILOT_SEED: u64 = 0x5eed_dc00;
const PILOT_EXECUTIONS: u64 = 500;
const PILOT_RETAINED_NUMERATOR: usize = 1;
const PILOT_RETAINED_DENOMINATOR: usize = 5;
const M13_SEEDS: [u64; 6] = [
    0x5eed_e000,
    0x5eed_e001,
    0x5eed_e002,
    0x5eed_e003,
    0x5eed_e004,
    0x5eed_e005,
];
const M13_EXECUTION_BUDGET: u64 = 5_000;
const M13_VALIDATION_SEED: u64 = 0x5eed_ef00;
const M13_VALIDATION_EXECUTIONS: u64 = 256;

#[derive(Debug, Deserialize, Serialize)]
struct M5Report {
    rom_sha256: String,
    ratchet: Vec<SmbCampaignReport>,
}

#[derive(Debug, Serialize)]
struct M6Report {
    rom_sha256: String,
    execution_budget: u64,
    seeds: Vec<u64>,
    source_m5_report: PathBuf,
    source_corpus_seed: u64,
    triage_calls: usize,
    triage_failures: u64,
    detector_decision: InstrumentorDecision,
    macro_decision: InstrumentorDecision,
    base_restart: Vec<SmbConfiguredReport>,
    luna_triage: Vec<SmbConfiguredReport>,
    luna_detectors: Vec<SmbConfiguredReport>,
    full_stack: Vec<SmbConfiguredReport>,
    detector_replay_verified: bool,
    full_stack_replay_verified: bool,
    baseline_max_scroll_bucket: u16,
    baseline_reached_flag: bool,
    baseline_reached_1_2: bool,
    full_stack_reaches_new_milestone: bool,
    full_stack_beats_baseline_time_to_milestone: bool,
}

#[derive(Debug, Serialize)]
struct M13Report {
    rom_sha256: String,
    source_archive: PathBuf,
    source_film_manifest: PathBuf,
    source_film_video: PathBuf,
    ranking_decision: InstrumentorDecision,
    validation_seed: u64,
    validation_executions: u64,
    execution_budget: u64,
    seeds: Vec<u64>,
    controls: Vec<SmbArchiveReport>,
    rankings: Vec<SmbArchiveReport>,
    replay_verified: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let first = args.next().ok_or("missing output directory or m12 mode")?;
    if first == "m12" {
        return run_m12(&mut args);
    }
    if first == "m13" {
        return run_m13(&mut args);
    }
    let output = PathBuf::from(first);
    let m5_path = required_path(&mut args, "M5 report")?;
    let triage_agent = required_path(&mut args, "triage-agent binary")?;
    let instrumentor_agent = required_path(&mut args, "instrumentor-agent binary")?;
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    fs::create_dir(&output)?;

    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(&rom_path)?;
    let m5: M5Report = read_json(&m5_path)?;
    let source_run = m5
        .ratchet
        .iter()
        .max_by_key(|run| {
            (
                run.milestones.reached_onward,
                run.milestones.reached_1_2,
                run.milestones.reached_1_1_flag,
                run.milestones.max_1_1_scroll_bucket,
            )
        })
        .ok_or("M5 report contains no ratchet runs")?;
    if source_run.corpus.len() > MAX_TRIAGE_CALLS {
        return Err("M5 source corpus exceeds the 200-call triage budget".into());
    }

    let operator_view = output.join("operator-view");
    let corpus_view = operator_view.join("corpus");
    fs::create_dir_all(&corpus_view)?;
    write_operator_scaffold(&operator_view, &m5, source_run)?;
    let triage_records = output.join("model-records/triage");
    let (labeled_corpus, triage_failures) = label_corpus(
        &rom,
        source_run,
        &operator_view,
        &triage_records,
        &triage_agent,
    )?;
    let labeled_path = output.join("labeled-corpus.json");
    write_json(&labeled_path, &labeled_corpus)?;

    let neutral_corpus = labeled_corpus
        .iter()
        .map(|entry| SmbLabeledCorpusEntry {
            input: entry.input.clone(),
            labels: neutral_labels(),
        })
        .collect::<Vec<_>>();
    let mut base_restart = Vec::new();
    let mut luna_triage = Vec::new();
    for seed in SEEDS {
        base_restart.push(run_smb_restart_configured(
            &rom,
            &neutral_corpus,
            seed,
            EXECUTION_BUDGET,
            NullSmbDetector,
            NullSmbMacro,
            no_artifacts(),
        )?);
        luna_triage.push(run_smb_restart_configured(
            &rom,
            &labeled_corpus,
            seed,
            EXECUTION_BUDGET,
            NullSmbDetector,
            NullSmbMacro,
            no_artifacts(),
        )?);
    }
    write_json(&output.join("base-restart.json"), &base_restart)?;
    write_json(&output.join("luna-triage.json"), &luna_triage)?;
    let pilot_control = run_smb_restart_configured(
        &rom,
        &labeled_corpus,
        PILOT_SEED,
        PILOT_EXECUTIONS,
        NullSmbDetector,
        NullSmbMacro,
        no_artifacts(),
    )?;
    write_json(&output.join("m12-pilot-control.json"), &pilot_control)?;

    let instrumentor_records = output.join("model-records/instrumentor");
    write_detector_interface(&operator_view)?;
    let detector_decision = obtain_artifact(
        ArtifactKind::Detector,
        1,
        (
            &operator_view,
            &instrumentor_records,
            &instrumentor_agent,
            &output,
        ),
        None,
        (&rom_path, &labeled_path, &pilot_control),
    )?
    .ok_or("detector artifact exhausted the predeclared attempts")?;
    write_macro_interface(&operator_view)?;
    let macro_decision = obtain_artifact(
        ArtifactKind::Macro,
        2,
        (
            &operator_view,
            &instrumentor_records,
            &instrumentor_agent,
            &output,
        ),
        Some(&detector_decision),
        (&rom_path, &labeled_path, &pilot_control),
    )?
    .ok_or("macro artifact exhausted the predeclared attempts")?;
    let generated_binary = build_generated(
        &output,
        &detector_decision.rust_source,
        &macro_decision.rust_source,
        "final",
    )
    .map_err(|error| format!("final generated-artifact build failed: {error}"))?;
    run_checked(
        Command::new(&generated_binary)
            .arg("verify")
            .arg(&rom_path)
            .arg(&labeled_path),
        &output.join("fixture-final"),
    )?;

    let mut luna_detectors = Vec::new();
    let mut full_stack = Vec::new();
    for seed in SEEDS {
        let detector_path = output.join(format!("detector-{seed:016x}.json"));
        run_generated(
            &generated_binary,
            "detector",
            &rom_path,
            &labeled_path,
            &detector_path,
            seed,
        )?;
        luna_detectors.push(read_json(&detector_path)?);

        let full_path = output.join(format!("full-{seed:016x}.json"));
        run_generated(
            &generated_binary,
            "full",
            &rom_path,
            &labeled_path,
            &full_path,
            seed,
        )?;
        full_stack.push(read_json(&full_path)?);
    }

    let detector_replay_path = output.join("detector-replay.json");
    run_generated(
        &generated_binary,
        "detector",
        &rom_path,
        &labeled_path,
        &detector_replay_path,
        SEEDS[0],
    )?;
    let detector_replay: SmbConfiguredReport = read_json(&detector_replay_path)?;
    let detector_replay_verified = detector_replay == luna_detectors[0];
    let full_replay_path = output.join("full-replay.json");
    run_generated(
        &generated_binary,
        "full",
        &rom_path,
        &labeled_path,
        &full_replay_path,
        SEEDS[0],
    )?;
    let full_replay: SmbConfiguredReport = read_json(&full_replay_path)?;
    let full_stack_replay_verified = full_replay == full_stack[0];

    let baseline_max_scroll_bucket = m5
        .ratchet
        .iter()
        .map(|run| run.milestones.max_1_1_scroll_bucket)
        .max()
        .unwrap_or(0);
    let baseline_reached_flag = m5.ratchet.iter().any(|run| run.milestones.reached_1_1_flag);
    let baseline_reached_1_2 = m5.ratchet.iter().any(|run| run.milestones.reached_1_2);
    let full_stack_reaches_new_milestone = full_stack.iter().any(|run| {
        (!baseline_reached_1_2 && run.campaign.milestones.reached_1_2)
            || (!baseline_reached_flag && run.campaign.milestones.reached_1_1_flag)
            || run.campaign.milestones.max_1_1_scroll_bucket > baseline_max_scroll_bucket
    });
    let full_stack_beats_baseline_time_to_milestone = full_stack_reaches_new_milestone;
    let report = M6Report {
        rom_sha256: m5.rom_sha256,
        execution_budget: EXECUTION_BUDGET,
        seeds: SEEDS.to_vec(),
        source_m5_report: m5_path,
        source_corpus_seed: source_run.seed,
        triage_calls: labeled_corpus.len(),
        triage_failures,
        detector_decision,
        macro_decision,
        base_restart,
        luna_triage,
        luna_detectors,
        full_stack,
        detector_replay_verified,
        full_stack_replay_verified,
        baseline_max_scroll_bucket,
        baseline_reached_flag,
        baseline_reached_1_2,
        full_stack_reaches_new_milestone,
        full_stack_beats_baseline_time_to_milestone,
    };
    write_json(&output.join("smb-m6-report.json"), &report)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report.triage_failures != 0 {
        return Err("M6 triage recorded model failures".into());
    }
    if !report.detector_replay_verified || !report.full_stack_replay_verified {
        return Err("M6 no-model replay mismatch".into());
    }
    if !report.full_stack_reaches_new_milestone {
        return Err("M6 full stack did not reach a milestone absent from M5".into());
    }
    Ok(())
}

fn run_m12(args: &mut impl Iterator<Item = std::ffi::OsString>) -> Result<(), Box<dyn Error>> {
    let output = required_path(args, "M12 output directory")?;
    let m10_report_path = required_path(args, "M10 live report")?;
    let film_manifest = required_path(args, "M10 film manifest")?;
    let film_video = required_path(args, "M10 film video")?;
    let labeled_path = required_path(args, "labeled corpus")?;
    let instrumentor_agent = required_path(args, "instrumentor-agent binary")?;
    if args.next().is_some() {
        return Err("unexpected extra M12 argument".into());
    }
    fs::create_dir(&output)?;
    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(&rom_path)?;
    let m10: SmbConfiguredReport = read_json(&m10_report_path)?;
    let labeled_corpus: Vec<SmbLabeledCorpusEntry> = read_json(&labeled_path)?;
    let operator_view = output.join("operator-view");
    fs::create_dir_all(operator_view.join("corpus"))?;
    write_m12_evidence(
        &operator_view,
        &rom,
        &m10,
        &m10_report_path,
        &film_manifest,
        &film_video,
    )?;

    let pilot_control = run_smb_restart_configured(
        &rom,
        &labeled_corpus,
        PILOT_SEED,
        PILOT_EXECUTIONS,
        NullSmbDetector,
        NullSmbMacro,
        no_artifacts(),
    )?;
    write_json(&output.join("m12-pilot-control.json"), &pilot_control)?;
    let records = output.join("model-records/instrumentor");
    write_detector_interface(&operator_view)?;
    let detector = obtain_artifact(
        ArtifactKind::Detector,
        1,
        (&operator_view, &records, &instrumentor_agent, &output),
        None,
        (&rom_path, &labeled_path, &pilot_control),
    )?;
    write_macro_interface(&operator_view)?;
    let macro_decision = obtain_artifact(
        ArtifactKind::Macro,
        2,
        (&operator_view, &records, &instrumentor_agent, &output),
        detector.as_ref(),
        (&rom_path, &labeled_path, &pilot_control),
    )?;

    let final_binary = if detector.is_some() || macro_decision.is_some() {
        let detector_source = match &detector {
            Some(decision) => decision.rust_source.as_str(),
            None => stub_detector_source(),
        };
        let macro_source = match &macro_decision {
            Some(decision) => decision.rust_source.as_str(),
            None => stub_macro_source(),
        };
        let binary = build_generated(&output, detector_source, macro_source, "m12-final")
            .map_err(|error| format!("M12 final generated-artifact build failed: {error}"))?;
        run_checked(
            Command::new(&binary)
                .arg("verify")
                .arg(&rom_path)
                .arg(&labeled_path),
            &output.join("artifact-validation/m12-final-fixture"),
        )?;
        Some(binary)
    } else {
        None
    };
    write_json(
        &output.join("m12-report.json"),
        &serde_json::json!({
            "source_m10_report": m10_report_path,
            "source_film_manifest": film_manifest,
            "source_film_video": film_video,
            "pilot_seed": PILOT_SEED,
            "pilot_executions": PILOT_EXECUTIONS,
            "retained_fraction_cap": {
                "numerator": PILOT_RETAINED_NUMERATOR,
                "denominator": PILOT_RETAINED_DENOMINATOR,
            },
            "max_attempts_per_invocation": MAX_ATTEMPTS,
            "detector": detector,
            "macro": macro_decision,
            "final_binary": final_binary,
        }),
    )?;
    Ok(())
}

fn run_m13(args: &mut impl Iterator<Item = std::ffi::OsString>) -> Result<(), Box<dyn Error>> {
    let output = required_path(args, "M13 output directory")?;
    let source_archive_path = required_path(args, "source archive report")?;
    let film_manifest = required_path(args, "plateau film manifest")?;
    let film_video = required_path(args, "plateau film video")?;
    let instrumentor_agent = required_path(args, "instrumentor-agent binary")?;
    if args.next().is_some() {
        return Err("unexpected extra M13 argument".into());
    }
    let resume_prepared_evidence = output.is_dir();
    if !resume_prepared_evidence {
        fs::create_dir(&output)?;
    }
    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(&rom_path)?;
    let source_archive: SmbArchiveReport = read_json(&source_archive_path)?;
    let operator_view = output.join("operator-view");
    if resume_prepared_evidence {
        validate_prepared_m13_evidence(&operator_view, &output)?;
    } else {
        fs::create_dir_all(operator_view.join("corpus"))?;
        write_m13_evidence(
            &operator_view,
            &rom,
            &source_archive,
            &film_manifest,
            &film_video,
        )?;
        write_ranking_interface(&operator_view)?;
    }
    let records = output.join("model-records/instrumentor");
    let ranking_decision = obtain_ranking(
        &operator_view,
        &records,
        &instrumentor_agent,
        &output,
        &rom_path,
        &source_archive_path,
    )?
    .ok_or("ranking artifact exhausted the predeclared attempts")?;
    let binary = build_generated_ranking(&output, &ranking_decision.rust_source, "m13-final")
        .map_err(|error| format!("M13 final ranking build failed: {error}"))?;
    run_checked(
        Command::new(&binary)
            .arg("verify")
            .arg(&rom_path)
            .arg(&source_archive_path),
        &output.join("artifact-validation/m13-final-fixture"),
    )?;

    let mut controls = Vec::new();
    let mut rankings = Vec::new();
    for seed in M13_SEEDS {
        let control_path = output.join(format!("control-{seed:016x}.json"));
        run_generated_ranking(
            &binary,
            "control",
            &rom_path,
            &source_archive_path,
            &control_path,
            seed,
            M13_EXECUTION_BUDGET,
        )?;
        controls.push(read_json(&control_path)?);
        let ranking_path = output.join(format!("ranking-{seed:016x}.json"));
        run_generated_ranking(
            &binary,
            "ranking",
            &rom_path,
            &source_archive_path,
            &ranking_path,
            seed,
            M13_EXECUTION_BUDGET,
        )?;
        rankings.push(read_json(&ranking_path)?);
    }
    let replay_path = output.join("ranking-replay.json");
    run_generated_ranking(
        &binary,
        "ranking",
        &rom_path,
        &source_archive_path,
        &replay_path,
        M13_SEEDS[0],
        M13_EXECUTION_BUDGET,
    )?;
    let replay: SmbArchiveReport = read_json(&replay_path)?;
    let replay_verified = replay == rankings[0];
    let report = M13Report {
        rom_sha256: format!("{:x}", Sha256::digest(&rom)),
        source_archive: source_archive_path,
        source_film_manifest: film_manifest,
        source_film_video: film_video,
        ranking_decision,
        validation_seed: M13_VALIDATION_SEED,
        validation_executions: M13_VALIDATION_EXECUTIONS,
        execution_budget: M13_EXECUTION_BUDGET,
        seeds: M13_SEEDS.to_vec(),
        controls,
        rankings,
        replay_verified,
    };
    write_json(&output.join("m13-report.json"), &report)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.replay_verified {
        return Err("M13 no-model archive replay mismatch".into());
    }
    Ok(())
}

fn validate_prepared_m13_evidence(view: &Path, output: &Path) -> Result<(), Box<dyn Error>> {
    for relative in [
        "fuzzer_stats",
        "source-summary.json",
        "plateau-film.json",
        "plateau-film.mp4",
        "artifact-interface.txt",
    ] {
        if !view.join(relative).is_file() {
            return Err(format!("prepared M13 evidence is missing {relative}").into());
        }
    }
    for sample in 0..8 {
        if !view
            .join("corpus")
            .join(format!("state-{sample:04}.json"))
            .is_file()
        {
            return Err("prepared M13 evidence is missing a state trace".into());
        }
    }
    if output.join("model-records").exists() || output.join("artifact-validation").exists() {
        return Err("M13 evidence resume refuses a started model or validation record".into());
    }
    Ok(())
}

fn write_m13_evidence(
    view: &Path,
    rom: &[u8],
    archive: &SmbArchiveReport,
    film_manifest: &Path,
    film_video: &Path,
) -> Result<(), Box<dyn Error>> {
    fs::write(
        view.join("fuzzer_stats"),
        format!(
            "target : nes-super-mario-bros\nexecs_done : {}\ncorpus_count : {}\n",
            archive.executions,
            archive.entries.len(),
        ),
    )?;
    write_json(
        &view.join("source-summary.json"),
        &serde_json::json!({
            "seed": archive.seed,
            "executions": archive.executions,
            "retained": archive.retained,
            "rejected": archive.rejected,
            "deaths": archive.deaths,
        }),
    )?;
    fs::copy(film_manifest, view.join("plateau-film.json"))?;
    fs::copy(film_video, view.join("plateau-film.mp4"))?;
    let sample_count = archive.entries.len().min(8);
    if sample_count == 0 {
        return Err("M13 source archive contains no entries".into());
    }
    for sample in 0..sample_count {
        let index = sample
            .checked_mul(archive.entries.len())
            .ok_or("M13 evidence index overflow")?
            / sample_count;
        let entry = &archive.entries[index];
        write_json(
            &view.join("corpus").join(format!("state-{sample:04}.json")),
            &serde_json::json!({
                "archive_id": entry.id,
                "observations": observe_smb_input(rom, &entry.input)?,
            }),
        )?;
    }
    Ok(())
}

fn obtain_ranking(
    operator_view: &Path,
    records: &Path,
    agent: &Path,
    output: &Path,
    rom: &Path,
    archive: &Path,
) -> Result<Option<InstrumentorDecision>, Box<dyn Error>> {
    let mut previous_error = None;
    for attempt in 1..=MAX_ATTEMPTS {
        let request = InstrumentorRequest {
            trial: 3,
            attempt,
            previous_error: previous_error.clone(),
        };
        let decision = call_json_agent::<_, InstrumentorDecision>(
            agent,
            &[
                OsStr::new("--operator-view"),
                operator_view.as_os_str(),
                OsStr::new("--records-dir"),
                records.as_os_str(),
            ],
            &request,
        )?;
        record_attempted_source(
            output,
            ArtifactKind::Ranking,
            3,
            attempt,
            &decision.rust_source,
        )?;
        if let Err(error) = validate_artifact(ArtifactKind::Ranking, &decision) {
            record_ranking_validation(output, attempt, false, Some(&error), None, None)?;
            previous_error = Some(error);
            continue;
        }
        match build_generated_ranking(
            output,
            &decision.rust_source,
            &format!("ranking-trial-3-attempt-{attempt}"),
        ) {
            Ok(binary) => match validate_built_ranking(&binary, rom, archive, output, attempt) {
                Ok(()) => return Ok(Some(decision)),
                Err(error) => {
                    let record = output
                        .join("artifact-validation")
                        .join(format!("ranking-trial-3-attempt-{attempt}.validation.json"));
                    if !record.exists() {
                        record_ranking_validation(
                            output,
                            attempt,
                            false,
                            Some(&error),
                            None,
                            None,
                        )?;
                    }
                    previous_error = Some(error);
                }
            },
            Err(error) => {
                record_ranking_validation(output, attempt, false, Some(&error), None, None)?;
                previous_error = Some(error);
            }
        }
    }
    Ok(None)
}

fn validate_built_ranking(
    binary: &Path,
    rom: &Path,
    archive: &Path,
    output: &Path,
    attempt: u8,
) -> Result<(), String> {
    let stem = format!("ranking-trial-3-attempt-{attempt}");
    if let Err(error) = run_checked(
        Command::new(binary).arg("verify").arg(rom).arg(archive),
        &output
            .join("artifact-validation")
            .join(format!("{stem}-fixture")),
    ) {
        let error = error.to_string();
        record_ranking_validation(output, attempt, false, Some(&error), None, None)
            .map_err(|record_error| record_error.to_string())?;
        return Err(error);
    }
    let control_path = output
        .join("artifact-validation")
        .join(format!("{stem}-control.json"));
    run_generated_ranking(
        binary,
        "control",
        rom,
        archive,
        &control_path,
        M13_VALIDATION_SEED,
        M13_VALIDATION_EXECUTIONS,
    )
    .map_err(|error| error.to_string())?;
    let candidate_path = output
        .join("artifact-validation")
        .join(format!("{stem}-candidate.json"));
    run_generated_ranking(
        binary,
        "ranking",
        rom,
        archive,
        &candidate_path,
        M13_VALIDATION_SEED,
        M13_VALIDATION_EXECUTIONS,
    )
    .map_err(|error| error.to_string())?;
    let replay_path = output
        .join("artifact-validation")
        .join(format!("{stem}-replay.json"));
    run_generated_ranking(
        binary,
        "ranking",
        rom,
        archive,
        &replay_path,
        M13_VALIDATION_SEED,
        M13_VALIDATION_EXECUTIONS,
    )
    .map_err(|error| error.to_string())?;
    let control: SmbArchiveReport = read_json(&control_path).map_err(|error| error.to_string())?;
    let candidate: SmbArchiveReport =
        read_json(&candidate_path).map_err(|error| error.to_string())?;
    let replay: SmbArchiveReport = read_json(&replay_path).map_err(|error| error.to_string())?;
    let validation = if control.seed != M13_VALIDATION_SEED
        || candidate.seed != M13_VALIDATION_SEED
        || control.executions != M13_VALIDATION_EXECUTIONS
        || candidate.executions != M13_VALIDATION_EXECUTIONS
    {
        Err("ranking pilot did not use the predeclared seed and execution budget".to_owned())
    } else if control.ranking.installed || !candidate.ranking.installed {
        Err("ranking pilot did not isolate the replacement policy".to_owned())
    } else if candidate != replay {
        Err("ranking pilot did not reproduce the exact archive report".to_owned())
    } else {
        Ok(())
    };
    match validation {
        Ok(()) => {
            record_ranking_validation(
                output,
                attempt,
                true,
                None,
                Some(&control),
                Some(&candidate),
            )
            .map_err(|error| error.to_string())?;
            Ok(())
        }
        Err(error) => {
            record_ranking_validation(
                output,
                attempt,
                false,
                Some(&error),
                Some(&control),
                Some(&candidate),
            )
            .map_err(|record_error| record_error.to_string())?;
            Err(error)
        }
    }
}

fn record_ranking_validation(
    output: &Path,
    attempt: u8,
    accepted: bool,
    error: Option<&str>,
    control: Option<&SmbArchiveReport>,
    candidate: Option<&SmbArchiveReport>,
) -> Result<(), Box<dyn Error>> {
    let directory = output.join("artifact-validation");
    fs::create_dir_all(&directory)?;
    write_json(
        &directory.join(format!("ranking-trial-3-attempt-{attempt}.validation.json")),
        &serde_json::json!({
            "kind": "ranking",
            "trial": 3,
            "attempt": attempt,
            "accepted": accepted,
            "error": error,
            "validation_seed": M13_VALIDATION_SEED,
            "validation_executions": M13_VALIDATION_EXECUTIONS,
            "control": control.map(archive_pilot_summary),
            "candidate": candidate.map(archive_pilot_summary),
        }),
    )
}

fn archive_pilot_summary(report: &SmbArchiveReport) -> serde_json::Value {
    serde_json::json!({
        "seed": report.seed,
        "executions": report.executions,
        "entries": report.entries.len(),
        "retained": report.retained,
        "rejected": report.rejected,
        "ranking": report.ranking,
    })
}

fn write_m12_evidence(
    view: &Path,
    rom: &[u8],
    report: &SmbConfiguredReport,
    report_path: &Path,
    film_manifest: &Path,
    film_video: &Path,
) -> Result<(), Box<dyn Error>> {
    fs::write(
        view.join("fuzzer_stats"),
        format!(
            "target : nes-super-mario-bros\nexecs_done : {}\ncorpus_count : {}\nmax_position_bucket : {}\nflag_observed : {}\nlevel_1_2_observed : {}\n",
            report.campaign.executions,
            report.campaign.corpus.len(),
            report.campaign.milestones.max_1_1_scroll_bucket,
            report.campaign.milestones.reached_1_1_flag,
            report.campaign.milestones.reached_1_2,
        ),
    )?;
    fs::write(
        view.join("input-vocabulary.txt"),
        "Inputs are ordered lists of NES controller chords. buttons is the standard eight-bit A/B/Select/Start/Up/Down/Left/Right mask. hold_frames is total and clamped to 1..=120. The host mutators append, perturb, truncate, and splice bounded lists of at most 96 chords.\n",
    )?;
    fs::write(
        view.join("observation-format.txt"),
        "Each observer event exposes frame_count, complete 2048-byte CPU work RAM, sorted changed_indices, terminal dead, and a mechanical log line. Events occur at each 16-pixel x transition, first death, and action endpoint.\n",
    )?;
    fs::copy(report_path, view.join("m10-live.json"))?;
    fs::copy(film_manifest, view.join("m10-max-scroll-film.json"))?;
    fs::copy(film_video, view.join("m10-max-scroll.mp4"))?;

    let mut traces = Vec::with_capacity(report.campaign.corpus.len());
    for (testcase_id, input) in report.campaign.corpus.iter().enumerate() {
        let observations = observe_smb_input(rom, input)?;
        let mut max_x = 0_u16;
        for observation in &observations {
            let wram = observation
                .wram
                .as_slice()
                .try_into()
                .map_err(|_| "M10 evidence observation WRAM is not exactly 2 KiB")?;
            max_x =
                max_x.max(fuzzer::phase4b::smb_milestones_from_wram(wram).max_1_1_scroll_bucket);
        }
        let dead = observations
            .last()
            .is_some_and(|observation| observation.dead);
        traces.push((max_x, dead, testcase_id, input.clone(), observations));
    }
    traces.sort_by_key(|(max_x, _, testcase_id, _, _)| (Reverse(*max_x), *testcase_id));
    let mut selected = traces
        .iter()
        .take(8)
        .map(|(_, _, testcase_id, _, _)| *testcase_id)
        .collect::<BTreeSet<_>>();
    selected.extend(
        traces
            .iter()
            .filter(|(_, dead, _, _, _)| *dead)
            .take(8)
            .map(|(_, _, testcase_id, _, _)| *testcase_id),
    );
    let mut index = Vec::new();
    for (max_x, dead, testcase_id, input, observations) in traces {
        if !selected.contains(&testcase_id) {
            continue;
        }
        write_json(
            &view
                .join("corpus")
                .join(format!("testcase-{testcase_id:020}.json")),
            &SmbTriageRequest {
                testcase_id: u64::try_from(testcase_id)?,
                execution_count: report.campaign.executions,
                input,
                observations,
            },
        )?;
        index.push(serde_json::json!({
            "testcase_id": testcase_id,
            "max_x": max_x,
            "ends_in_death": dead,
        }));
    }
    write_json(&view.join("evidence-index.json"), &index)?;
    Ok(())
}

fn write_operator_scaffold(
    view: &Path,
    m5: &M5Report,
    source: &SmbCampaignReport,
) -> Result<(), Box<dyn Error>> {
    fs::write(
        view.join("fuzzer_stats"),
        format!(
            "target : nes-super-mario-bros\nexecs_done : {}\ncorpus_count : {}\nmax_position_bucket : {}\nflag_observed : {}\nlevel_1_2_observed : {}\n",
            source.executions,
            source.corpus.len(),
            source.milestones.max_1_1_scroll_bucket,
            source.milestones.reached_1_1_flag,
            source.milestones.reached_1_2,
        ),
    )?;
    fs::write(
        view.join("plot_data"),
        format!(
            "# deterministic; no wall-clock columns\nexecs_done,corpus_count,max_position_bucket\n{},{},{}\n",
            source.executions,
            source.corpus.len(),
            source.milestones.max_1_1_scroll_bucket,
        ),
    )?;
    fs::write(
        view.join("input-vocabulary.txt"),
        "Inputs are ordered lists of NES controller chords. buttons is the standard eight-bit A/B/Select/Start/Up/Down/Left/Right mask. hold_frames is total and clamped to 1..=120. The host mutators append, perturb, truncate, and splice bounded lists of at most 96 chords.\n",
    )?;
    fs::write(
        view.join("observation-format.txt"),
        "Each observer event exposes frame_count, the complete 2048-byte CPU work RAM as an integer array, sorted changed_indices, a terminal dead bit, and a mechanical log line containing only frame count and changed indices. Events occur at each 16-pixel x transition, first death, and action endpoint. No other RAM offset is decoded or declared to mean progress. The base novelty map is deliberately position-only.\n",
    )?;
    fs::write(view.join("m5-summary.json"), serde_json::to_vec_pretty(m5)?)?;
    Ok(())
}

fn label_corpus(
    rom: &[u8],
    source: &SmbCampaignReport,
    operator_view: &Path,
    records: &Path,
    agent: &Path,
) -> Result<(Vec<SmbLabeledCorpusEntry>, u64), Box<dyn Error>> {
    let mut result = Vec::new();
    let mut failures = 0_u64;
    for (index, input) in source.corpus.iter().enumerate() {
        let observations = observe_smb_input(rom, input)?;
        let request = SmbTriageRequest {
            testcase_id: index as u64,
            execution_count: 0,
            input: input.clone(),
            observations,
        };
        let stem = format!("testcase-{index:020}");
        write_json(
            &operator_view.join("corpus").join(format!("{stem}.json")),
            &request,
        )?;
        let labels = match call_json_agent::<_, TriageLabels>(
            agent,
            &[
                OsStr::new("--operator-view"),
                operator_view.as_os_str(),
                OsStr::new("--records-dir"),
                records.as_os_str(),
            ],
            &request,
        ) {
            Ok(labels) => labels,
            Err(error) => {
                failures = failures.saturating_add(1);
                fs::write(
                    operator_view
                        .join("corpus")
                        .join(format!("{stem}.failure.txt")),
                    error.to_string(),
                )?;
                neutral_labels()
            }
        };
        write_json(
            &operator_view
                .join("corpus")
                .join(format!("{stem}.labels.json")),
            &labels,
        )?;
        result.push(SmbLabeledCorpusEntry {
            input: input.clone(),
            labels,
        });
    }
    Ok((result, failures))
}

fn neutral_labels() -> TriageLabels {
    TriageLabels {
        interest: Interest::Neutral,
        duplicate_of: None,
        flags: Vec::<Flag>::new(),
        tags: Vec::new(),
        summary: "neutral host fallback".to_owned(),
        hypotheses: Vec::new(),
    }
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

#[derive(Clone, Copy, Debug)]
enum ArtifactKind {
    Detector,
    Macro,
    Ranking,
}

fn obtain_artifact(
    kind: ArtifactKind,
    trial: u8,
    paths: (&Path, &Path, &Path, &Path),
    detector: Option<&InstrumentorDecision>,
    pilot: (&Path, &Path, &SmbConfiguredReport),
) -> Result<Option<InstrumentorDecision>, Box<dyn Error>> {
    let (operator_view, records, agent, output) = paths;
    let (rom, corpus, pilot_control) = pilot;
    let mut previous_error = None;
    for attempt in 1..=MAX_ATTEMPTS {
        let request = InstrumentorRequest {
            trial,
            attempt,
            previous_error: previous_error.clone(),
        };
        let decision = call_json_agent::<_, InstrumentorDecision>(
            agent,
            &[
                OsStr::new("--operator-view"),
                operator_view.as_os_str(),
                OsStr::new("--records-dir"),
                records.as_os_str(),
            ],
            &request,
        )?;
        record_attempted_source(output, kind, trial, attempt, &decision.rust_source)?;
        if let Err(error) = validate_artifact(kind, &decision) {
            record_validation(
                output,
                kind,
                (trial, attempt),
                false,
                Some(&error),
                None,
                None,
            )?;
            previous_error = Some(error);
            continue;
        }
        let detector_source = detector.map_or_else(
            || {
                if matches!(kind, ArtifactKind::Detector) {
                    decision.rust_source.as_str()
                } else {
                    stub_detector_source()
                }
            },
            |value| value.rust_source.as_str(),
        );
        let macro_source = if matches!(kind, ArtifactKind::Macro) {
            decision.rust_source.as_str()
        } else {
            stub_macro_source()
        };
        match build_generated(
            output,
            detector_source,
            macro_source,
            &format!("trial-{trial}-attempt-{attempt}"),
        ) {
            Ok(binary) => match validate_built_artifact(
                kind,
                &binary,
                (rom, corpus),
                output,
                (trial, attempt),
                pilot_control,
            ) {
                Ok(()) => return Ok(Some(decision)),
                Err(error) => {
                    if !validation_record_path(output, kind, trial, attempt).exists() {
                        record_validation(
                            output,
                            kind,
                            (trial, attempt),
                            false,
                            Some(&error),
                            None,
                            None,
                        )?;
                    }
                    previous_error = Some(error);
                }
            },
            Err(error) => {
                record_validation(
                    output,
                    kind,
                    (trial, attempt),
                    false,
                    Some(&error),
                    None,
                    None,
                )?;
                previous_error = Some(error);
            }
        }
    }
    Ok(None)
}

fn validate_built_artifact(
    kind: ArtifactKind,
    binary: &Path,
    inputs: (&Path, &Path),
    output: &Path,
    invocation: (u8, u8),
    detector_control: &SmbConfiguredReport,
) -> Result<(), String> {
    let (rom, corpus) = inputs;
    let (trial, attempt) = invocation;
    let stem = format!("{}-trial-{trial}-attempt-{attempt}", artifact_name(kind));
    if let Err(error) = run_checked(
        Command::new(binary).arg("verify").arg(rom).arg(corpus),
        &output
            .join("artifact-validation")
            .join(format!("{stem}-fixture")),
    ) {
        let error = error.to_string();
        record_validation(
            output,
            kind,
            (trial, attempt),
            false,
            Some(&error),
            None,
            None,
        )
        .map_err(|record_error| record_error.to_string())?;
        return Err(error);
    }

    let control_path = output
        .join("artifact-validation")
        .join(format!("{stem}-control.json"));
    let control = if matches!(kind, ArtifactKind::Detector) {
        write_json(&control_path, detector_control).map_err(|error| error.to_string())?;
        detector_control.clone()
    } else {
        run_generated(binary, "detector", rom, corpus, &control_path, PILOT_SEED)
            .map_err(|error| error.to_string())?;
        read_json(&control_path).map_err(|error| error.to_string())?
    };
    let candidate_path = output
        .join("artifact-validation")
        .join(format!("{stem}-candidate.json"));
    let candidate_arm = if matches!(kind, ArtifactKind::Detector) {
        "detector"
    } else {
        "full"
    };
    run_generated(
        binary,
        candidate_arm,
        rom,
        corpus,
        &candidate_path,
        PILOT_SEED,
    )
    .map_err(|error| error.to_string())?;
    let candidate: SmbConfiguredReport =
        read_json(&candidate_path).map_err(|error| error.to_string())?;
    let initial_corpus_count: usize = read_json::<Vec<SmbLabeledCorpusEntry>>(corpus)
        .map_err(|error| error.to_string())?
        .len();
    let pilot = validate_pilot(&control, &candidate, initial_corpus_count);
    match pilot {
        Ok(()) => {
            record_validation(
                output,
                kind,
                (trial, attempt),
                true,
                None,
                Some(&control),
                Some(&candidate),
            )
            .map_err(|error| error.to_string())?;
            Ok(())
        }
        Err(error) => {
            record_validation(
                output,
                kind,
                (trial, attempt),
                false,
                Some(&error),
                Some(&control),
                Some(&candidate),
            )
            .map_err(|record_error| record_error.to_string())?;
            Err(error)
        }
    }
}

fn validate_pilot(
    control: &SmbConfiguredReport,
    candidate: &SmbConfiguredReport,
    initial_corpus_count: usize,
) -> Result<(), String> {
    if control.campaign.seed != PILOT_SEED
        || candidate.campaign.seed != PILOT_SEED
        || control.campaign.executions != PILOT_EXECUTIONS
        || candidate.campaign.executions != PILOT_EXECUTIONS
    {
        return Err(
            "artifact pilot did not use the predeclared seed and 500-execution budget".to_owned(),
        );
    }
    validate_pilot_metrics(
        initial_corpus_count,
        candidate.campaign.corpus.len(),
        candidate.campaign.executions,
        control.campaign.milestones.max_1_1_scroll_bucket,
        candidate.campaign.milestones.max_1_1_scroll_bucket,
    )
}

fn validate_pilot_metrics(
    initial_corpus_count: usize,
    candidate_corpus_count: usize,
    executions: u64,
    control_max_x: u16,
    candidate_max_x: u16,
) -> Result<(), String> {
    let retained = candidate_corpus_count
        .checked_sub(initial_corpus_count)
        .ok_or_else(|| "artifact pilot lost restored corpus entries".to_owned())?;
    let execution_count = usize::try_from(executions)
        .map_err(|_| "artifact pilot execution count exceeds usize".to_owned())?;
    let retained_scaled = retained
        .checked_mul(PILOT_RETAINED_DENOMINATOR)
        .ok_or_else(|| "artifact pilot retention calculation overflowed".to_owned())?;
    let cap_scaled = execution_count
        .checked_mul(PILOT_RETAINED_NUMERATOR)
        .ok_or_else(|| "artifact pilot cap calculation overflowed".to_owned())?;
    if retained_scaled > cap_scaled {
        return Err(format!(
            "artifact pilot retained {retained}/{execution_count} executions; cap is {PILOT_RETAINED_NUMERATOR}/{PILOT_RETAINED_DENOMINATOR}"
        ));
    }
    if candidate_max_x < control_max_x {
        return Err(format!(
            "artifact pilot max x regressed from {control_max_x} to {candidate_max_x}"
        ));
    }
    Ok(())
}

fn record_attempted_source(
    output: &Path,
    kind: ArtifactKind,
    trial: u8,
    attempt: u8,
    source: &str,
) -> Result<(), Box<dyn Error>> {
    let directory = output.join("artifact-validation");
    fs::create_dir_all(&directory)?;
    fs::write(
        directory.join(format!(
            "{}-trial-{trial}-attempt-{attempt}.rs",
            artifact_name(kind)
        )),
        with_license(source),
    )?;
    Ok(())
}

fn record_validation(
    output: &Path,
    kind: ArtifactKind,
    invocation: (u8, u8),
    accepted: bool,
    error: Option<&str>,
    control: Option<&SmbConfiguredReport>,
    candidate: Option<&SmbConfiguredReport>,
) -> Result<(), Box<dyn Error>> {
    let (trial, attempt) = invocation;
    let directory = output.join("artifact-validation");
    fs::create_dir_all(&directory)?;
    write_json(
        &validation_record_path(output, kind, trial, attempt),
        &serde_json::json!({
            "kind": artifact_name(kind),
            "trial": trial,
            "attempt": attempt,
            "accepted": accepted,
            "error": error,
            "pilot_seed": PILOT_SEED,
            "pilot_executions": PILOT_EXECUTIONS,
            "retained_fraction_cap": {
                "numerator": PILOT_RETAINED_NUMERATOR,
                "denominator": PILOT_RETAINED_DENOMINATOR,
            },
            "control": control.map(pilot_summary),
            "candidate": candidate.map(pilot_summary),
        }),
    )
}

fn validation_record_path(output: &Path, kind: ArtifactKind, trial: u8, attempt: u8) -> PathBuf {
    output.join("artifact-validation").join(format!(
        "{}-trial-{trial}-attempt-{attempt}.validation.json",
        artifact_name(kind)
    ))
}

fn pilot_summary(report: &SmbConfiguredReport) -> serde_json::Value {
    serde_json::json!({
        "seed": report.campaign.seed,
        "executions": report.campaign.executions,
        "corpus_count": report.campaign.corpus.len(),
        "max_x": report.campaign.milestones.max_1_1_scroll_bucket,
        "reached_flag": report.campaign.milestones.reached_1_1_flag,
        "reached_1_2": report.campaign.milestones.reached_1_2,
    })
}

fn artifact_name(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Detector => "detector",
        ArtifactKind::Macro => "macro",
        ArtifactKind::Ranking => "ranking",
    }
}

fn validate_artifact(kind: ArtifactKind, decision: &InstrumentorDecision) -> Result<(), String> {
    let (action, marker) = match kind {
        ArtifactKind::Detector => (
            InstrumentorAction::InstallDetector,
            "SmbDetector for InstalledDetector",
        ),
        ArtifactKind::Macro => (
            InstrumentorAction::InstallMutator,
            "SmbMacro for InstalledMacro",
        ),
        ArtifactKind::Ranking => (
            InstrumentorAction::InstallRanking,
            "SmbRanking for InstalledRanking",
        ),
    };
    if decision.action != action {
        return Err(format!(
            "expected {action:?}, received {:?}",
            decision.action
        ));
    }
    if decision.scope_to_lineage.is_some() {
        return Err("SMB generated artifacts are global; scope_to_lineage must be null".to_owned());
    }
    if decision.rust_source.is_empty() || decision.rust_source.len() > SOURCE_LIMIT {
        return Err("source is empty or exceeds 256 KiB".to_owned());
    }
    if !decision.rust_source.contains(marker) {
        return Err(format!(
            "source does not implement required marker {marker:?}"
        ));
    }
    for forbidden in [
        "unsafe",
        "std::fs",
        "std::process",
        "std::net",
        "std::thread",
        "std::env",
        "Command",
        "include!",
        "include_str!",
        "include_bytes!",
        "extern crate",
        "#[link",
        "asm!",
        "panic!",
        "unreachable!",
        "todo!",
        "unimplemented!",
        ".unwrap(",
        ".expect(",
        "loop {",
        "while ",
    ] {
        if decision.rust_source.contains(forbidden) {
            return Err(format!("source contains forbidden token {forbidden:?}"));
        }
    }
    if matches!(kind, ArtifactKind::Ranking) {
        let lowercase = decision.rust_source.to_ascii_lowercase();
        for forbidden in [
            "progress",
            "milestone",
            "world",
            "level",
            "flag",
            "0x071a",
            "0x71a",
            "1818",
            "0x071c",
            "0x71c",
            "1820",
            "0x075f",
            "0x75f",
            "1887",
            "0x075c",
            "0x75c",
            "1884",
            "0x0746",
            "0x746",
            "1862",
        ] {
            if lowercase.contains(forbidden) {
                return Err(format!(
                    "ranking source contains forbidden progress token {forbidden:?}"
                ));
            }
        }
    }
    Ok(())
}

fn write_detector_interface(view: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        view.join("artifact-interface.txt"),
        "This invocation asks for a detector. Return action=install_detector. Complete source declares `pub struct InstalledDetector;` and implements `fuzzer::phase4b::SmbDetector` for it. The method is `fn features(&self, observations: &[fuzzer::phase4b::SmbObservations]) -> Vec<u64>`. Each observation exposes only frame_count, wram, changed_indices, dead, and log_line. Feature keys are global and reduced modulo 4096 by the host, so preserve useful conjunctions with distinct low bits. Source is pure, deterministic, bounded, and uses no dependencies beyond fuzzer/std.\n",
    )?;
    Ok(())
}

fn write_macro_interface(view: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        view.join("artifact-interface.txt"),
        "This invocation asks for a generated semantic mutator such as a parameterized jump arc. Return action=install_mutator. Complete source declares `pub struct InstalledMacro;` and implements `fuzzer::phase4b::SmbMacro` for it. The method is `fn mutate(&self, input: &fuzzer::phase4b::SmbInput, seed: u64) -> fuzzer::phase4b::SmbInput`. It may import `ButtonChord`, `SmbInput`, `MAX_HOLD_FRAMES`, and `MAX_SMB_ACTIONS`. The host draws seed from LibAFL's seeded RNG, so the same input and seed must return the same candidate while different seeds may select bounded parameter variants. The result must contain at most 96 chords and every duration must be in 1..=120. Source is pure, deterministic, bounded, and uses no dependencies beyond fuzzer/std. A generated macro must be meaningfully parameterized by the visible input and/or seed rather than merely copying it.\n",
    )?;
    Ok(())
}

fn write_ranking_interface(view: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        view.join("artifact-interface.txt"),
        "This invocation asks for a generated archive ranking. Return action=install_ranking. Complete source declares `pub struct InstalledRanking;` and implements `fuzzer::phase4c::SmbRanking` for it. The method is `fn score(&self, observations: &[fuzzer::phase4b::SmbObservations]) -> i64`. It is one pure, deterministic, bounded function over one state's recorded observations and may combine several terms into one comparable score. Do not use progress measures; cell keys and frontier selection already represent them. Choose the ranking from the supplied corpus evidence only. The host consults it only when a full cell considers replacement and keeps fewer actions as the final tie-breaker. Source uses no dependencies beyond fuzzer/std.\n",
    )?;
    Ok(())
}

fn build_generated_ranking(
    output: &Path,
    ranking_source: &str,
    stem: &str,
) -> Result<PathBuf, String> {
    let build = output.join("build").join(stem);
    let source = build.join("src");
    fs::create_dir_all(&source).map_err(|error| error.to_string())?;
    let fuzzer_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let escaped = fuzzer_path
        .to_str()
        .ok_or_else(|| "fuzzer path is not UTF-8".to_owned())?
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    fs::write(
        build.join("Cargo.toml"),
        format!(
            "[package]\nname = \"m13-installed\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"AGPL-3.0-or-later\"\n\n[dependencies]\nfuzzer = {{ path = \"{escaped}\" }}\nserde_json = \"1.0\"\n\n[workspace]\n"
        ),
    )
    .map_err(|error| error.to_string())?;
    fs::write(source.join("ranking.rs"), with_license(ranking_source))
        .map_err(|error| error.to_string())?;
    fs::write(source.join("main.rs"), generated_ranking_main_source())
        .map_err(|error| error.to_string())?;
    let target = output.join("build/target");
    let result = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("--quiet")
        .arg("--offline")
        .arg("--manifest-path")
        .arg(build.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .output()
        .map_err(|error| error.to_string())?;
    fs::write(output.join(format!("build-{stem}.stdout")), &result.stdout)
        .map_err(|error| error.to_string())?;
    fs::write(output.join(format!("build-{stem}.stderr")), &result.stderr)
        .map_err(|error| error.to_string())?;
    if !result.status.success() {
        return Err(bounded_lossy(&result.stderr));
    }
    Ok(target.join("release/m13-installed"))
}

fn generated_ranking_main_source() -> &'static str {
    r#"// SPDX-License-Identifier: AGPL-3.0-or-later

mod ranking;

use std::{error::Error, fs, path::PathBuf};
use fuzzer::{
    phase4b::{SmbInput, observe_smb_input},
    phase4c::{
        MAX_SMB_COMPLETION_ACTIONS, SmbArchiveDurationPolicy, SmbArchiveReport,
        SmbArchiveSuffixPolicy, SmbRanking, SmbRankingSearchConfig,
        run_smb_archive_search_with_config,
        run_smb_archive_search_with_ranking,
    },
};
use ranking::InstalledRanking;

fn sampled_inputs(report: &SmbArchiveReport) -> Result<Vec<SmbInput>, Box<dyn Error>> {
    let frontier = report
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
        .max()
        .ok_or("source archive contains no entries")?;
    let input = report
        .entries
        .iter()
        .filter(|entry| (entry.key.world, entry.key.level, entry.key.progress) == frontier)
        .min_by_key(|entry| (entry.input.actions.len(), entry.id))
        .ok_or("source archive contains no frontier input")?
        .input
        .clone();
    Ok(vec![input])
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let mode = args.next().ok_or("missing mode")?;
    let rom_path = PathBuf::from(args.next().ok_or("missing ROM path")?);
    let archive_path = PathBuf::from(args.next().ok_or("missing archive path")?);
    let rom = fs::read(rom_path)?;
    let source: SmbArchiveReport = serde_json::from_slice(&fs::read(archive_path)?)?;
    let inputs = sampled_inputs(&source)?;
    match mode.to_str() {
        Some("verify") => {
            if args.next().is_some() {
                return Err("unexpected verify argument".into());
            }
            let empty_a = InstalledRanking.score(&[]);
            let empty_b = InstalledRanking.score(&[]);
            if empty_a != empty_b {
                return Err("generated ranking was nondeterministic on empty evidence".into());
            }
            for input in &inputs {
                let first = observe_smb_input(&rom, input)?;
                let second = observe_smb_input(&rom, input)?;
                if first != second {
                    return Err("recorded state observations were nondeterministic".into());
                }
                if InstalledRanking.score(&first) != InstalledRanking.score(&second) {
                    return Err("generated ranking was nondeterministic".into());
                }
            }
            Ok(())
        }
        Some("run") => {
            let arm = args.next().ok_or("missing arm")?;
            let output = PathBuf::from(args.next().ok_or("missing output report")?);
            let seed: u64 = args.next().ok_or("missing seed")?.to_string_lossy().parse()?;
            let budget: u64 = args.next().ok_or("missing budget")?.to_string_lossy().parse()?;
            if args.next().is_some() {
                return Err("unexpected run argument".into());
            }
            let report = match arm.to_str() {
                Some("control") => run_smb_archive_search_with_config(
                    &rom,
                    &inputs,
                    seed,
                    budget,
                    MAX_SMB_COMPLETION_ACTIONS,
                    SmbArchiveDurationPolicy::Stratified,
                )?,
                Some("ranking") => run_smb_archive_search_with_ranking(
                    &rom,
                    &inputs,
                    seed,
                    budget,
                    SmbRankingSearchConfig {
                        max_actions: MAX_SMB_COMPLETION_ACTIONS,
                        duration_policy: SmbArchiveDurationPolicy::Stratified,
                        suffix_policy: SmbArchiveSuffixPolicy::OneOrTwo,
                    },
                    &InstalledRanking,
                )?,
                _ => return Err("unknown ranking arm".into()),
            };
            fs::write(output, serde_json::to_vec_pretty(&report)?)?;
            Ok(())
        }
        _ => Err("unknown mode".into()),
    }
}
"#
}

fn run_generated_ranking(
    binary: &Path,
    arm: &str,
    rom: &Path,
    archive: &Path,
    output: &Path,
    seed: u64,
    budget: u64,
) -> Result<(), Box<dyn Error>> {
    run_checked(
        Command::new(binary)
            .arg("run")
            .arg(rom)
            .arg(archive)
            .arg(arm)
            .arg(output)
            .arg(seed.to_string())
            .arg(budget.to_string()),
        &output.with_extension("process"),
    )
}

fn build_generated(
    output: &Path,
    detector_source: &str,
    macro_source: &str,
    stem: &str,
) -> Result<PathBuf, String> {
    let build = output.join("build").join(stem);
    let source = build.join("src");
    fs::create_dir_all(&source).map_err(|error| error.to_string())?;
    let fuzzer_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let escaped = fuzzer_path
        .to_str()
        .ok_or_else(|| "fuzzer path is not UTF-8".to_owned())?
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    fs::write(
        build.join("Cargo.toml"),
        format!(
            "[package]\nname = \"m6-installed\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"AGPL-3.0-or-later\"\n\n[dependencies]\nfuzzer = {{ path = \"{escaped}\" }}\nserde_json = \"1.0\"\n\n[workspace]\n"
        ),
    )
    .map_err(|error| error.to_string())?;
    fs::write(source.join("detector.rs"), with_license(detector_source))
        .map_err(|error| error.to_string())?;
    fs::write(
        source.join("generated_macro.rs"),
        with_license(macro_source),
    )
    .map_err(|error| error.to_string())?;
    fs::write(source.join("main.rs"), generated_main_source())
        .map_err(|error| error.to_string())?;
    let target = output.join("build/target");
    let result = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("--quiet")
        .arg("--offline")
        .arg("--manifest-path")
        .arg(build.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target)
        .output()
        .map_err(|error| error.to_string())?;
    fs::write(output.join(format!("build-{stem}.stdout")), &result.stdout)
        .map_err(|error| error.to_string())?;
    fs::write(output.join(format!("build-{stem}.stderr")), &result.stderr)
        .map_err(|error| error.to_string())?;
    if !result.status.success() {
        return Err(bounded_lossy(&result.stderr));
    }
    Ok(target.join("release/m6-installed"))
}

fn generated_main_source() -> &'static str {
    "// SPDX-License-Identifier: AGPL-3.0-or-later\n\nmod detector;\nmod generated_macro;\n\nuse std::{error::Error, fs, path::PathBuf};\nuse detector::InstalledDetector;\nuse generated_macro::InstalledMacro;\nuse fuzzer::phase4b::{MAX_HOLD_FRAMES, MAX_SMB_ACTIONS, NullSmbMacro, SmbArtifactConfig, SmbDetector, SmbLabeledCorpusEntry, SmbMacro, observe_smb_input, run_smb_restart_configured};\n\nconst MAX_GENERATED_FEATURES: usize = 4096;\nconst VERIFY_SEEDS: [u64; 3] = [0, 0x5eed_dc00, u64::MAX];\n\nfn main() -> Result<(), Box<dyn Error>> {\n    let mut args = std::env::args_os().skip(1);\n    let mode = args.next().ok_or(\"missing mode\")?;\n    let rom_path = PathBuf::from(args.next().ok_or(\"missing ROM path\")?);\n    let corpus_path = PathBuf::from(args.next().ok_or(\"missing corpus path\")?);\n    let rom = fs::read(rom_path)?;\n    let corpus: Vec<SmbLabeledCorpusEntry> = serde_json::from_slice(&fs::read(corpus_path)?)?;\n    match mode.to_str() {\n        Some(\"verify\") => {\n            if args.next().is_some() { return Err(\"unexpected verify argument\".into()); }\n            for entry in &corpus {\n                let first = observe_smb_input(&rom, &entry.input)?;\n                let second = observe_smb_input(&rom, &entry.input)?;\n                if first != second { return Err(\"fixture RAM trace was nondeterministic\".into()); }\n                let features_a = InstalledDetector.features(&first);\n                let features_b = InstalledDetector.features(&second);\n                if features_a != features_b { return Err(\"generated detector was nondeterministic\".into()); }\n                if features_a.len() > MAX_GENERATED_FEATURES { return Err(\"generated detector exceeded the feature bound\".into()); }\n                for seed in VERIFY_SEEDS {\n                    let a = InstalledMacro.mutate(&entry.input, seed);\n                    let b = InstalledMacro.mutate(&entry.input, seed);\n                    if a != b || a.actions.len() > MAX_SMB_ACTIONS || a.actions.iter().any(|chord| chord.hold_frames == 0 || chord.hold_frames > MAX_HOLD_FRAMES) { return Err(\"generated macro violated deterministic bounds\".into()); }\n                }\n            }\n            Ok(())\n        }\n        Some(\"run\") => {\n            let arm = args.next().ok_or(\"missing arm\")?;\n            let output = PathBuf::from(args.next().ok_or(\"missing output report\")?);\n            let seed: u64 = args.next().ok_or(\"missing seed\")?.to_string_lossy().parse()?;\n            let budget: u64 = args.next().ok_or(\"missing budget\")?.to_string_lossy().parse()?;\n            if args.next().is_some() { return Err(\"unexpected run argument\".into()); }\n            let report = match arm.to_str() {\n                Some(\"detector\") => run_smb_restart_configured(&rom, &corpus, seed, budget, InstalledDetector, NullSmbMacro, SmbArtifactConfig { detector_name: \"luna_smb_detector\", detector_retire_after: 128, macro_name: \"none\", macro_retire_after: u64::MAX, enable_macro: false })?,\n                Some(\"full\") => run_smb_restart_configured(&rom, &corpus, seed, budget, InstalledDetector, InstalledMacro, SmbArtifactConfig { detector_name: \"luna_smb_detector\", detector_retire_after: 128, macro_name: \"luna_smb_macro\", macro_retire_after: 128, enable_macro: true })?,\n                _ => return Err(\"unknown arm\".into()),\n            };\n            fs::write(output, serde_json::to_vec_pretty(&report)?)?;\n            Ok(())\n        }\n        _ => Err(\"unknown mode\".into()),\n    }\n}\n"
}

fn stub_detector_source() -> &'static str {
    "pub struct InstalledDetector;\nimpl fuzzer::phase4b::SmbDetector for InstalledDetector { fn features(&self, _observations: &[fuzzer::phase4b::SmbObservations]) -> Vec<u64> { Vec::new() } }\n"
}

fn stub_macro_source() -> &'static str {
    "pub struct InstalledMacro;\nimpl fuzzer::phase4b::SmbMacro for InstalledMacro { fn mutate(&self, input: &fuzzer::phase4b::SmbInput, _seed: u64) -> fuzzer::phase4b::SmbInput { input.clone() } }\n"
}

fn with_license(source: &str) -> String {
    if source.starts_with("// SPDX-License-Identifier:") {
        source.to_owned()
    } else {
        format!("// SPDX-License-Identifier: AGPL-3.0-or-later\n\n{source}")
    }
}

fn run_generated(
    binary: &Path,
    arm: &str,
    rom: &Path,
    corpus: &Path,
    output: &Path,
    seed: u64,
) -> Result<(), Box<dyn Error>> {
    run_checked(
        Command::new(binary)
            .arg("run")
            .arg(rom)
            .arg(corpus)
            .arg(arm)
            .arg(output)
            .arg(seed.to_string())
            .arg(EXECUTION_BUDGET.to_string()),
        &output.with_extension("process"),
    )
}

fn run_checked(command: &mut Command, record: &Path) -> Result<(), Box<dyn Error>> {
    let result = command.output()?;
    fs::write(record.with_extension("stdout"), &result.stdout)?;
    fs::write(record.with_extension("stderr"), &result.stderr)?;
    if !result.status.success() {
        return Err(format!(
            "subprocess failed with {}: {}",
            result.status,
            bounded_lossy(&result.stderr)
        )
        .into());
    }
    Ok(())
}

fn call_json_agent<I, O>(program: &Path, args: &[&OsStr], input: &I) -> Result<O, Box<dyn Error>>
where
    I: Serialize,
    O: DeserializeOwned,
{
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let bytes = serde_json::to_vec(input)?;
    child
        .stdin
        .take()
        .ok_or("agent subprocess has no stdin")?
        .write_all(&bytes)?;
    let result = child.wait_with_output()?;
    if !result.status.success() {
        return Err(format!(
            "agent failed with {}: {}",
            result.status,
            bounded_lossy(&result.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&result.stdout)?)
}

fn bounded_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(ERROR_LIMIT)
        .collect()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), Box<dyn Error>> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

fn read_json<T>(path: &Path) -> Result<T, Box<dyn Error>>
where
    T: DeserializeOwned,
{
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn required_path(
    args: &mut impl Iterator<Item = std::ffi::OsString>,
    name: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    Ok(PathBuf::from(
        args.next().ok_or_else(|| format!("missing {name}"))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::{ArtifactKind, validate_artifact, validate_pilot_metrics};
    use fuzzer::phase4a::{InstrumentorAction, InstrumentorDecision};

    #[test]
    fn m6_whole_wram_shotgun_fixture_is_rejected() {
        let error = validate_pilot_metrics(39, 371, 500, 44, 48)
            .expect_err("66.4% retained executions must exceed the 20% cap");
        assert!(error.contains("retained 332/500"));
    }

    #[test]
    fn pilot_rejects_max_x_regression_at_an_acceptable_retention_rate() {
        let error = validate_pilot_metrics(39, 50, 500, 49, 48)
            .expect_err("candidate must not regress max x");
        assert!(error.contains("regressed from 49 to 48"));
    }

    #[test]
    fn pilot_accepts_the_predeclared_cap_without_max_x_regression() {
        validate_pilot_metrics(39, 139, 500, 49, 49).expect("20% cap is inclusive");
    }

    #[test]
    fn smb_artifacts_reject_legacy_lineage_scoping() {
        let decision = InstrumentorDecision {
            action: InstrumentorAction::InstallDetector,
            name: "scoped".to_owned(),
            rust_source: "pub struct InstalledDetector; impl fuzzer::phase4b::SmbDetector for InstalledDetector { fn features(&self, _: &[fuzzer::phase4b::SmbObservations]) -> Vec<u64> { Vec::new() } }".to_owned(),
            scope_to_lineage: Some(7),
            rationale: "fixture".to_owned(),
        };
        let error = validate_artifact(ArtifactKind::Detector, &decision)
            .expect_err("SMB detector must remain global");
        assert!(error.contains("scope_to_lineage must be null"));
    }

    #[test]
    fn ranking_rejects_progress_terms() {
        let decision = InstrumentorDecision {
            action: InstrumentorAction::InstallRanking,
            name: "progress_rank".to_owned(),
            rust_source: "pub struct InstalledRanking; impl fuzzer::phase4c::SmbRanking for InstalledRanking { fn score(&self, observations: &[fuzzer::phase4b::SmbObservations]) -> i64 { observations.last().map_or(0, |event| i64::from(event.wram[0x071a])) } }".to_owned(),
            scope_to_lineage: None,
            rationale: "fixture".to_owned(),
        };
        let error = validate_artifact(ArtifactKind::Ranking, &decision)
            .expect_err("ranking must not duplicate progress measures");
        assert!(error.contains("forbidden progress token"));
    }
}
