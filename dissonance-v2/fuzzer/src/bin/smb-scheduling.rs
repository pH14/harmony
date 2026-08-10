// SPDX-License-Identifier: AGPL-3.0-or-later

//! M9 synchronous SMB label-refresh smoke and exact no-model replay.

use std::{
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
    phase4b::{
        NullSmbDetector, NullSmbMacro, SmbArtifactConfig, SmbConfiguredReport,
        SmbLabeledCorpusEntry, SmbTriageRequest, observe_smb_input, replay_smb_restart_configured,
        run_smb_restart_configured_with_triage,
    },
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

const SEED: u64 = 0x5eed_d900;
const EXECUTION_BUDGET: u64 = 1_000;
const ERROR_LIMIT: usize = 16_384;

#[derive(Debug, Deserialize)]
struct Args {
    output: PathBuf,
    corpus: PathBuf,
    triage_agent: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
struct SchedulingReport {
    rom_sha256: String,
    seed: u64,
    execution_budget: u64,
    triage_mode: &'static str,
    triage_calls: usize,
    triage_failures: u64,
    replay_verified: bool,
    live: SmbConfiguredReport,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = parse_args()?;
    fs::create_dir(&args.output)?;
    let operator_view = args.output.join("operator-view");
    let corpus_view = operator_view.join("corpus");
    let records = args.output.join("model-records/triage");
    fs::create_dir_all(&corpus_view)?;
    write_operator_scaffold(&operator_view)?;

    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(&rom_path)?;
    let initial_corpus: Vec<SmbLabeledCorpusEntry> = read_json(&args.corpus)?;
    populate_operator_corpus(&rom, &initial_corpus, &corpus_view)?;
    let mut triage_failures = 0_u64;
    let mut triage_calls = 0_usize;
    let live = {
        let mut triage = |request: &SmbTriageRequest| {
            let stem = format!("testcase-{:020}", request.testcase_id);
            fs::write(
                operator_view.join("fuzzer_stats"),
                format!(
                    "target : nes-super-mario-bros\nexecs_done : {}\ncorpus_count : {}\nlabel_refresh_executions : 500\nboost_multiplier : 4\n",
                    request.execution_count,
                    request.testcase_id.saturating_add(1),
                ),
            )?;
            write_json(&corpus_view.join(format!("{stem}.json")), request)?;
            let labels = match &args.triage_agent {
                Some(agent) => match call_json_agent::<_, TriageLabels>(
                    agent,
                    &[
                        OsStr::new("--operator-view"),
                        operator_view.as_os_str(),
                        OsStr::new("--records-dir"),
                        records.as_os_str(),
                    ],
                    request,
                ) {
                    Ok(labels) => labels,
                    Err(error) => {
                        triage_failures = triage_failures.saturating_add(1);
                        fs::write(
                            corpus_view.join(format!("{stem}.failure.txt")),
                            error.to_string(),
                        )?;
                        neutral_labels()
                    }
                },
                None => neutral_labels(),
            };
            triage_calls = triage_calls.saturating_add(1);
            write_json(&corpus_view.join(format!("{stem}.labels.json")), &labels)?;
            Ok(labels)
        };
        run_smb_restart_configured_with_triage(
            &rom,
            &initial_corpus,
            SEED,
            EXECUTION_BUDGET,
            NullSmbDetector,
            NullSmbMacro,
            no_artifacts(),
            &mut triage,
        )
    };
    let live = live?;
    write_json(&args.output.join("live.json"), &live)?;

    let replay = replay_smb_restart_configured(
        &rom,
        &initial_corpus,
        SEED,
        EXECUTION_BUDGET,
        NullSmbDetector,
        NullSmbMacro,
        no_artifacts(),
        &live.label_events,
    )?;
    let replay_verified = replay == live;
    write_json(&args.output.join("replay.json"), &replay)?;
    let report = SchedulingReport {
        rom_sha256: format!("{:x}", Sha256::digest(&rom)),
        seed: SEED,
        execution_budget: EXECUTION_BUDGET,
        triage_mode: if args.triage_agent.is_some() {
            "luna"
        } else {
            "neutral-preflight"
        },
        triage_calls,
        triage_failures,
        replay_verified,
        live,
    };
    write_json(&args.output.join("smb-m9-report.json"), &report)?;
    println!(
        "M9 {}: {} executions, {} labels, max bucket {}, replay_verified={}",
        report.triage_mode,
        report.execution_budget,
        report.triage_calls,
        report.live.campaign.milestones.max_1_1_scroll_bucket,
        report.replay_verified,
    );
    if !report.replay_verified {
        return Err("M9 recorded-label replay diverged".into());
    }
    Ok(())
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let parsed = Args {
        output: PathBuf::from(args.next().ok_or("missing output directory")?),
        corpus: PathBuf::from(args.next().ok_or("missing labeled corpus")?),
        triage_agent: match args
            .next()
            .ok_or("missing triage-agent binary or --neutral")?
        {
            value if value == "--neutral" => None,
            value => Some(PathBuf::from(value)),
        },
    };
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    Ok(parsed)
}

fn write_operator_scaffold(view: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        view.join("fuzzer_stats"),
        "target : nes-super-mario-bros\nlabel_refresh_executions : 500\nboost_multiplier : 4\n",
    )?;
    fs::write(
        view.join("input-vocabulary.txt"),
        "Inputs are ordered lists of NES controller chords. buttons is the standard eight-bit A/B/Select/Start/Up/Down/Left/Right mask. hold_frames is total and clamped to 1..=120. The host mutators append, perturb, truncate, and splice bounded lists of at most 96 chords.\n",
    )?;
    fs::write(
        view.join("observation-format.txt"),
        "Each retained corpus file contains its SmbInput and action-boundary observations. Each observation exposes frame_count, complete 2048-byte work RAM, sorted changed_indices, and its existing mechanical log_line. No separately joined log is supplied. No RAM offset is decoded or declared to mean progress.\n",
    )?;
    Ok(())
}

fn populate_operator_corpus(
    rom: &[u8],
    corpus: &[SmbLabeledCorpusEntry],
    view: &Path,
) -> Result<(), Box<dyn Error>> {
    for (index, entry) in corpus.iter().enumerate() {
        let testcase_id = u64::try_from(index)?;
        let stem = format!("testcase-{testcase_id:020}");
        write_json(
            &view.join(format!("{stem}.json")),
            &SmbTriageRequest {
                testcase_id,
                execution_count: 0,
                input: entry.input.clone(),
                observations: observe_smb_input(rom, &entry.input)?,
            },
        )?;
        write_json(&view.join(format!("{stem}.labels.json")), &entry.labels)?;
    }
    Ok(())
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
    child
        .stdin
        .take()
        .ok_or("agent subprocess has no stdin")?
        .write_all(&serde_json::to_vec(input)?)?;
    let result = child.wait_with_output()?;
    if !result.status.success() {
        return Err(format!(
            "agent failed with {}: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
                .chars()
                .take(ERROR_LIMIT)
                .collect::<String>()
        )
        .into());
    }
    Ok(serde_json::from_slice(&result.stdout)?)
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
