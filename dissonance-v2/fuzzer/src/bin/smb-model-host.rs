// SPDX-License-Identifier: AGPL-3.0-or-later

//! M6 live-model orchestration, generated-artifact validation, restart, and replay.

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
    phase4a::{InstrumentorAction, InstrumentorDecision, InstrumentorRequest},
    phase4b::{
        NullSmbDetector, NullSmbMacro, SmbArtifactConfig, SmbCampaignReport, SmbConfiguredReport,
        SmbLabeledCorpusEntry, SmbTriageRequest, observe_smb_input, run_smb_restart_configured,
    },
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

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

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let output = required_path(&mut args, "output directory")?;
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

    let instrumentor_records = output.join("model-records/instrumentor");
    write_detector_interface(&operator_view)?;
    let detector_decision = obtain_artifact(
        ArtifactKind::Detector,
        1,
        &operator_view,
        &instrumentor_records,
        &instrumentor_agent,
        &output,
        None,
    )?;
    write_macro_interface(&operator_view)?;
    let macro_decision = obtain_artifact(
        ArtifactKind::Macro,
        2,
        &operator_view,
        &instrumentor_records,
        &instrumentor_agent,
        &output,
        Some(&detector_decision),
    )?;
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
        "Each action boundary exposes frame_count, the complete 2048-byte CPU work RAM as an integer array, sorted changed_indices, and a mechanical log line containing only frame count and changed indices. No RAM offset is decoded or declared to mean progress. The base novelty map is deliberately coarse position only.\n",
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
        let log = observations
            .iter()
            .map(|observation| observation.log_line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let request = SmbTriageRequest {
            testcase_id: index as u64,
            observations,
            log,
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

#[derive(Clone, Copy)]
enum ArtifactKind {
    Detector,
    Macro,
}

fn obtain_artifact(
    kind: ArtifactKind,
    trial: u8,
    operator_view: &Path,
    records: &Path,
    agent: &Path,
    output: &Path,
    detector: Option<&InstrumentorDecision>,
) -> Result<InstrumentorDecision, Box<dyn Error>> {
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
        if let Err(error) = validate_artifact(kind, &decision) {
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
            Ok(_) => return Ok(decision),
            Err(error) => previous_error = Some(error),
        }
    }
    Err(format!("model artifact trial {trial} exhausted three attempts").into())
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
    };
    if decision.action != action {
        return Err(format!(
            "expected {action:?}, received {:?}",
            decision.action
        ));
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
    Ok(())
}

fn write_detector_interface(view: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        view.join("artifact-interface.txt"),
        "This invocation asks for a detector. Return action=install_detector. Complete source declares `pub struct InstalledDetector;` and implements `fuzzer::phase4b::SmbDetector` for it. The method is `fn features(&self, observations: &[fuzzer::phase4b::SmbObservations]) -> Vec<u64>`. Each observation exposes only frame_count, wram, changed_indices, and log_line. Feature keys are global and reduced modulo 4096 by the host, so preserve useful conjunctions with distinct low bits. Source is pure, deterministic, bounded, and uses no dependencies beyond fuzzer/std.\n",
    )?;
    Ok(())
}

fn write_macro_interface(view: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        view.join("artifact-interface.txt"),
        "This invocation asks for a generated semantic mutator such as a parameterized jump arc. Return action=install_mutator. Complete source declares `pub struct InstalledMacro;` and implements `fuzzer::phase4b::SmbMacro` for it. The method is `fn mutate(&self, input: &fuzzer::phase4b::SmbInput) -> fuzzer::phase4b::SmbInput`. It may import `ButtonChord`, `SmbInput`, `MAX_HOLD_FRAMES`, and `MAX_SMB_ACTIONS`. The result must contain at most 96 chords and durations at most 120. Source is pure, deterministic, bounded, and uses no dependencies beyond fuzzer/std. A generated macro must be meaningfully parameterized by the visible input rather than merely copying it.\n",
    )?;
    Ok(())
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
    "// SPDX-License-Identifier: AGPL-3.0-or-later\n\nmod detector;\nmod generated_macro;\n\nuse std::{error::Error, fs, path::PathBuf};\nuse detector::InstalledDetector;\nuse generated_macro::InstalledMacro;\nuse fuzzer::phase4b::{MAX_HOLD_FRAMES, MAX_SMB_ACTIONS, NullSmbMacro, SmbArtifactConfig, SmbDetector, SmbLabeledCorpusEntry, SmbMacro, observe_smb_input, run_smb_restart_configured};\n\nfn main() -> Result<(), Box<dyn Error>> {\n    let mut args = std::env::args_os().skip(1);\n    let mode = args.next().ok_or(\"missing mode\")?;\n    let rom_path = PathBuf::from(args.next().ok_or(\"missing ROM path\")?);\n    let corpus_path = PathBuf::from(args.next().ok_or(\"missing corpus path\")?);\n    let rom = fs::read(rom_path)?;\n    let corpus: Vec<SmbLabeledCorpusEntry> = serde_json::from_slice(&fs::read(corpus_path)?)?;\n    match mode.to_str() {\n        Some(\"verify\") => {\n            if args.next().is_some() { return Err(\"unexpected verify argument\".into()); }\n            for entry in &corpus {\n                let first = observe_smb_input(&rom, &entry.input)?;\n                let second = observe_smb_input(&rom, &entry.input)?;\n                if first != second { return Err(\"fixture RAM trace was nondeterministic\".into()); }\n                if InstalledDetector.features(&first) != InstalledDetector.features(&second) { return Err(\"generated detector was nondeterministic\".into()); }\n                let a = InstalledMacro.mutate(&entry.input);\n                let b = InstalledMacro.mutate(&entry.input);\n                if a != b || a.actions.len() > MAX_SMB_ACTIONS || a.actions.iter().any(|chord| chord.hold_frames == 0 || chord.hold_frames > MAX_HOLD_FRAMES) { return Err(\"generated macro violated deterministic bounds\".into()); }\n            }\n            Ok(())\n        }\n        Some(\"run\") => {\n            let arm = args.next().ok_or(\"missing arm\")?;\n            let output = PathBuf::from(args.next().ok_or(\"missing output report\")?);\n            let seed: u64 = args.next().ok_or(\"missing seed\")?.to_string_lossy().parse()?;\n            let budget: u64 = args.next().ok_or(\"missing budget\")?.to_string_lossy().parse()?;\n            if args.next().is_some() { return Err(\"unexpected run argument\".into()); }\n            let report = match arm.to_str() {\n                Some(\"detector\") => run_smb_restart_configured(&rom, &corpus, seed, budget, InstalledDetector, NullSmbMacro, SmbArtifactConfig { detector_name: \"luna_smb_detector\", detector_retire_after: 128, macro_name: \"none\", macro_retire_after: u64::MAX, enable_macro: false })?,\n                Some(\"full\") => run_smb_restart_configured(&rom, &corpus, seed, budget, InstalledDetector, InstalledMacro, SmbArtifactConfig { detector_name: \"luna_smb_detector\", detector_retire_after: 128, macro_name: \"luna_smb_macro\", macro_retire_after: 128, enable_macro: true })?,\n                _ => return Err(\"unknown arm\".into()),\n            };\n            fs::write(output, serde_json::to_vec_pretty(&report)?)?;\n            Ok(())\n        }\n        _ => Err(\"unknown mode\".into()),\n    }\n}\n"
}

fn stub_detector_source() -> &'static str {
    "pub struct InstalledDetector;\nimpl fuzzer::phase4b::SmbDetector for InstalledDetector { fn features(&self, _observations: &[fuzzer::phase4b::SmbObservations]) -> Vec<u64> { Vec::new() } }\n"
}

fn stub_macro_source() -> &'static str {
    "pub struct InstalledMacro;\nimpl fuzzer::phase4b::SmbMacro for InstalledMacro { fn mutate(&self, input: &fuzzer::phase4b::SmbInput) -> fuzzer::phase4b::SmbInput { input.clone() } }\n"
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
