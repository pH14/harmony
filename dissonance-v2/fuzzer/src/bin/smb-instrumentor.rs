// SPDX-License-Identifier: AGPL-3.0-or-later

//! The campaign-scale instrumentor loop host: `prove` the stall, `prepare`
//! the operator view, and `install` one authored attempt against it.
//!
//! This is the M2 `model-instrumentor` discipline at campaign scale. The
//! loop is level-triggered on the stall state: `prove` reads a finished
//! run's recorded outputs and decides "stalled" mechanically; `prepare`
//! refuses anything but a proven stall and assembles the operator view a
//! model session reads; `install` validates one authored artifact,
//! fixture-verifies its determinism, launches the next campaign with the
//! artifact active and header-recorded, then re-evaluates the stall proof.
//! A broken stall — or the third failed attempt — writes the escalation
//! record for the integrator.

use std::{env, error::Error, ffi::OsString, fs, path::PathBuf, process::Command};

use fuzzer::{
    campaign::{
        SmbCampaignArtifactRecord, SmbCampaignModeReport, scope_from_identifier, scope_identifier,
    },
    instrumentor::{
        SmbStallAttemptRecord, SmbStallEscalationRecord, assemble_smb_stall_operator_view,
        file_sha256, next_smb_stall_attempt, prove_smb_campaign_plateau, read_smb_stall_attempts,
        smb_stall_attempt_run_summary, write_smb_stall_escalation,
    },
    phase4a::{InstrumentorAction, InstrumentorDecision},
    phase4c::{SmbArchiveReport, SmbArtifactScope},
};

const GENERATED_SOURCE_LIMIT: usize = 262_144;
const BUILD_ERROR_LIMIT: usize = 16_384;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let command = args.next().ok_or("missing subcommand")?;
    match command.to_str() {
        Some("prove") => prove(&mut args),
        Some("prepare") => prepare(&mut args),
        Some("install") => install(&mut args),
        _ => Err("unknown smb-instrumentor subcommand".into()),
    }
}

/// Decide "stalled" from two recorded archives and emit the proof record.
///
/// Read-only over its inputs: it computes their hashes before and after and
/// refuses to report if either changed underneath it.
fn prove(args: &mut impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let origin_path = required_path(args, "origin archive")?;
    let produced_path = required_path(args, "produced archive")?;
    let proof_path = required_path(args, "proof output")?;
    reject_extra_args(args)?;
    let origin_sha = file_sha256(&origin_path)?;
    let produced_sha = file_sha256(&produced_path)?;
    let origin: SmbArchiveReport = serde_json::from_slice(&fs::read(&origin_path)?)?;
    let produced: SmbArchiveReport = serde_json::from_slice(&fs::read(&produced_path)?)?;
    let proof = prove_smb_campaign_plateau(&origin, &produced, &origin_sha, &produced_sha)?;
    fs::write(&proof_path, serde_json::to_vec_pretty(&proof)?)?;
    if file_sha256(&origin_path)? != origin_sha || file_sha256(&produced_path)? != produced_sha {
        return Err("a recorded artifact changed while the proof ran".into());
    }
    println!("stalled={}", proof.stalled);
    Ok(())
}

/// Assemble the operator view for one proven stall.
///
/// Arguments: stall directory, the stalled run's origin archive, the
/// stalled run's directory (`stream.jsonl` + `archive-live.json`), and an
/// optional rendered-film path.
fn prepare(args: &mut impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let stall_dir = required_path(args, "stall directory")?;
    let origin_path = required_path(args, "origin archive")?;
    let run_dir = required_path(args, "stalled run directory")?;
    let film = args
        .next()
        .map(|value| value.to_string_lossy().into_owned());
    reject_extra_args(args)?;
    let origin_sha = file_sha256(&origin_path)?;
    let produced_path = run_dir.join("archive-live.json");
    let produced_sha = file_sha256(&produced_path)?;
    let origin: SmbArchiveReport = serde_json::from_slice(&fs::read(&origin_path)?)?;
    let produced: SmbArchiveReport = serde_json::from_slice(&fs::read(&produced_path)?)?;
    let stream_text = fs::read_to_string(run_dir.join("stream.jsonl"))?;
    let attempts = read_smb_stall_attempts(&stall_dir)?;
    let proof = assemble_smb_stall_operator_view(
        &stall_dir.join("operator-view"),
        &origin,
        &origin_sha,
        &produced,
        &produced_sha,
        &stream_text,
        film.as_deref(),
        &attempts,
    )?;
    println!(
        "operator view assembled: stalled={} prior_attempts={}",
        proof.stalled,
        attempts.len(),
    );
    Ok(())
}

/// Which authored artifact kind one decision installs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ArtifactKind {
    Detector,
    Mutator,
}

impl ArtifactKind {
    fn label(self) -> &'static str {
        match self {
            Self::Detector => "detector",
            Self::Mutator => "macro",
        }
    }
}

/// Validate one campaign-scale instrumentor decision and return its kind
/// and declared scope.
fn validate_decision(
    decision: &InstrumentorDecision,
) -> Result<(ArtifactKind, SmbArtifactScope), Box<dyn Error>> {
    let kind = match decision.action {
        InstrumentorAction::InstallDetector => ArtifactKind::Detector,
        InstrumentorAction::InstallMutator => ArtifactKind::Mutator,
        _ => {
            return Err(format!(
                "campaign install supports only install_detector and install_mutator, not {:?}; \
                 policy-value attempts launch through the smb-campaign flags",
                decision.action
            )
            .into());
        }
    };
    if decision.scope_to_lineage.is_some() {
        return Err(
            "campaign artifacts are scoped by region: scope_to_lineage must be null".into(),
        );
    }
    let scope = scope_from_identifier(decision.scope.as_deref().ok_or(
        "campaign artifacts must declare a scope: world,level,progress_min,progress_max",
    )?)?;
    if decision.rust_source.is_empty() || decision.rust_source.len() > GENERATED_SOURCE_LIMIT {
        return Err("authored source is empty or exceeds 256 KiB".into());
    }
    let (facade_type, facade_impl) = match kind {
        ArtifactKind::Detector => (
            "pub struct InstalledDetector",
            "SmbDetector for InstalledDetector",
        ),
        ArtifactKind::Mutator => ("pub struct InstalledMacro", "SmbMacro for InstalledMacro"),
    };
    if !decision.rust_source.contains(facade_type) || !decision.rust_source.contains(facade_impl) {
        return Err(format!(
            "authored source does not implement the exact {} facade",
            kind.label()
        )
        .into());
    }
    for forbidden in [
        "unsafe",
        "std::fs",
        "std::process",
        "std::net",
        "std::thread",
        "std::env",
        "std::time",
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
        "File::",
        "OpenOptions",
        "Tcp",
        "Udp",
        "rand",
        "static mut",
    ] {
        if decision.rust_source.contains(forbidden) {
            return Err(format!("authored source contains forbidden token {forbidden:?}").into());
        }
    }
    Ok((kind, scope))
}

/// Install one authored attempt: validate, build, fixture-verify, launch
/// the next run with the artifact active, and re-evaluate the stall proof.
///
/// Arguments: stall directory, the stalled run's directory, the decision
/// file, then campaign seed, worker count, execution budget, and host name
/// for the attempt run. `--replay` additionally replays the attempt's
/// recorded stream through the installed binary.
fn install(args: &mut impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let stall_dir = required_path(args, "stall directory")?;
    let stalled_run_dir = required_path(args, "stalled run directory")?;
    let decision_path = required_path(args, "decision file")?;
    let campaign_seed: u64 = parse_u64(&required_string(args, "campaign seed")?)?;
    let workers: u64 = parse_u64(&required_string(args, "worker count")?)?;
    let execution_budget: u64 = parse_u64(&required_string(args, "execution budget")?)?;
    let host = required_string(args, "host name")?;
    let mut replay = false;
    for flag in args.by_ref() {
        if flag == "--replay" {
            replay = true;
        } else {
            return Err("unexpected install argument".into());
        }
    }

    // The three-attempt cap fires before any work: a capped stall escalates
    // instead of consuming a fourth run.
    let attempt = next_smb_stall_attempt(&stall_dir)?;
    let decision: InstrumentorDecision = serde_json::from_slice(&fs::read(&decision_path)?)?;
    let (kind, scope) = validate_decision(&decision)?;
    let artifact_name = format!("stall_attempt_{attempt}_{}", kind.label());

    let attempt_dir = stall_dir.join(format!("attempt-{attempt}"));
    fs::create_dir_all(&attempt_dir)?;
    fs::write(
        attempt_dir.join("decision.json"),
        serde_json::to_vec_pretty(&decision)?,
    )?;
    let authored_source = if decision
        .rust_source
        .starts_with("// SPDX-License-Identifier:")
    {
        decision.rust_source.clone()
    } else {
        format!(
            "// SPDX-License-Identifier: AGPL-3.0-or-later\n\n{}",
            decision.rust_source
        )
    };
    fs::write(attempt_dir.join("artifact.rs"), &authored_source)?;
    let source_sha256 = file_sha256(&attempt_dir.join("artifact.rs"))?;

    // Build the installed binary from the authored source, offline, in an
    // ignored crate, exactly as the M2 install harness does.
    let build_dir = attempt_dir.join("build/installed-campaign-artifact");
    let source_dir = build_dir.join("src");
    fs::create_dir_all(&source_dir)?;
    let dependency_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dependency_path = dependency_path
        .to_str()
        .ok_or("fuzzer dependency path is not UTF-8")?
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    fs::write(
        build_dir.join("Cargo.toml"),
        format!(
            "[package]\nname = \"installed-campaign-artifact\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"AGPL-3.0-or-later\"\n\n[dependencies]\nfuzzer = {{ path = \"{dependency_path}\" }}\nserde_json = \"1\"\nsha2 = \"0.10\"\n\n[workspace]\n"
        ),
    )?;
    fs::copy(
        attempt_dir.join("artifact.rs"),
        source_dir.join("artifact.rs"),
    )?;
    fs::write(
        source_dir.join("main.rs"),
        installed_main_source(kind, &artifact_name, &source_sha256, scope),
    )?;
    let target_dir = attempt_dir.join("build/target");
    let build = Command::new("cargo")
        .arg("build")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(build_dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        .env("CARGO_NET_OFFLINE", "true")
        .output()?;
    fs::write(attempt_dir.join("build.stdout"), &build.stdout)?;
    fs::write(attempt_dir.join("build.stderr"), &build.stderr)?;
    if !build.status.success() {
        return Err(format!(
            "authored artifact build failed: {}",
            bounded_lossy(&build.stderr)
        )
        .into());
    }
    let binary = target_dir.join("debug").join(if cfg!(windows) {
        "installed-campaign-artifact.exe"
    } else {
        "installed-campaign-artifact"
    });

    // Determinism fixture verify, non-negotiable before any launch: a
    // nondeterministic artifact silently poisons every stream recorded
    // with it.
    let verify = Command::new(&binary)
        .arg("verify")
        .arg(&stalled_run_dir)
        .output()?;
    fs::write(attempt_dir.join("verify.stdout"), &verify.stdout)?;
    fs::write(attempt_dir.join("verify.stderr"), &verify.stderr)?;
    if !verify.status.success() {
        return Err(format!(
            "authored artifact fixture verify failed: {}",
            bounded_lossy(&verify.stderr)
        )
        .into());
    }

    // Launch the next run with the artifact active: bounded budget,
    // header-recorded, from the stalled link's archive under its policies.
    let run_dir = attempt_dir.join("run");
    let run = Command::new(&binary)
        .arg("run")
        .arg(&stalled_run_dir)
        .arg(&run_dir)
        .arg(campaign_seed.to_string())
        .arg(workers.to_string())
        .arg(execution_budget.to_string())
        .arg(&host)
        .output()?;
    fs::write(attempt_dir.join("run.stdout"), &run.stdout)?;
    fs::write(attempt_dir.join("run.stderr"), &run.stderr)?;
    if !run.status.success() {
        return Err(format!(
            "installed campaign run failed: {}",
            bounded_lossy(&run.stderr)
        )
        .into());
    }
    if replay {
        let replayed = Command::new(&binary)
            .arg("replay")
            .arg(&run_dir)
            .arg(&stalled_run_dir)
            .output()?;
        fs::write(attempt_dir.join("replay.stdout"), &replayed.stdout)?;
        fs::write(attempt_dir.join("replay.stderr"), &replayed.stderr)?;
        if !replayed.status.success() {
            return Err(format!(
                "installed campaign replay diverged: {}",
                bounded_lossy(&replayed.stderr)
            )
            .into());
        }
    }

    // The loop's one exit test: does the stall proof still hold against the
    // attempt's own recorded outputs?
    let stalled_archive_path = stalled_run_dir.join("archive-live.json");
    let attempt_archive_path = run_dir.join("archive-live.json");
    let stalled_sha = file_sha256(&stalled_archive_path)?;
    let attempt_sha = file_sha256(&attempt_archive_path)?;
    let stalled_archive: SmbArchiveReport =
        serde_json::from_slice(&fs::read(&stalled_archive_path)?)?;
    let attempt_archive: SmbArchiveReport =
        serde_json::from_slice(&fs::read(&attempt_archive_path)?)?;
    let proof = prove_smb_campaign_plateau(
        &stalled_archive,
        &attempt_archive,
        &stalled_sha,
        &attempt_sha,
    )?;
    fs::write(
        attempt_dir.join("plateau-proof.json"),
        serde_json::to_vec_pretty(&proof)?,
    )?;
    let stall_broken = !proof.stalled;
    let report: SmbCampaignModeReport =
        serde_json::from_slice(&fs::read(run_dir.join("campaign-report.json"))?)?;
    let record = SmbStallAttemptRecord {
        attempt,
        action: match kind {
            ArtifactKind::Detector => "install_detector".to_owned(),
            ArtifactKind::Mutator => "install_mutator".to_owned(),
        },
        artifact: SmbCampaignArtifactRecord {
            name: artifact_name,
            source_sha256,
            scope: scope_identifier(scope),
        },
        rationale: decision.rationale.clone(),
        authored_source,
        run: smb_stall_attempt_run_summary(&report),
        proof: proof.clone(),
        stall_broken,
    };
    fs::write(
        attempt_dir.join(fuzzer::instrumentor::SMB_STALL_ATTEMPT_RECORD),
        serde_json::to_vec_pretty(&record)?,
    )?;

    let attempts = read_smb_stall_attempts(&stall_dir)?;
    if stall_broken {
        // Stall broken: escalate for a promotion ruling with every attempt
        // attached.
        write_smb_stall_escalation(
            &stall_dir,
            &SmbStallEscalationRecord {
                disposition: "stall_broken".to_owned(),
                proof,
                attempts,
            },
        )?;
        println!("stall_broken=true attempt={attempt} escalation=written");
    } else if attempt >= fuzzer::instrumentor::SMB_STALL_ATTEMPT_CAP {
        // Third failed attempt: escalate to the integrator with every
        // attempt attached, the same path the maze took manually.
        write_smb_stall_escalation(
            &stall_dir,
            &SmbStallEscalationRecord {
                disposition: "attempt_cap_reached".to_owned(),
                proof,
                attempts,
            },
        )?;
        println!("stall_broken=false attempt={attempt} escalation=written");
    } else {
        println!("stall_broken=false attempt={attempt} loop=re-fires");
    }
    Ok(())
}

/// The generated crate's main source: fixture verify, the installed
/// campaign run, and its exact replay, with the artifact's provenance
/// baked in as constants.
fn installed_main_source(
    kind: ArtifactKind,
    artifact_name: &str,
    source_sha256: &str,
    scope: SmbArtifactScope,
) -> String {
    let scope_identifier = scope_identifier(scope);
    let (artifact_use, artifact_binding, verify_artifact) = match kind {
        ArtifactKind::Detector => (
            "use artifact::InstalledDetector;",
            "let artifacts = SmbCampaignArtifacts { generated_mutator: None, generated_detector: Some(SmbCampaignGeneratedDetector { name: ARTIFACT_NAME.to_owned(), source_sha256: SOURCE_SHA256.to_owned(), scope, detector: &InstalledDetector }) };",
            "for entry in &sample {\n        let first = observe_smb_input(&rom, &entry.input)?;\n        let second = observe_smb_input(&rom, &entry.input)?;\n        if first != second { return Err(\"fixture RAM trace was nondeterministic\".into()); }\n        let features_a = InstalledDetector.features(&first);\n        let features_b = InstalledDetector.features(&second);\n        if features_a != features_b { return Err(\"authored detector was nondeterministic\".into()); }\n        if features_a.len() > 4096 { return Err(\"authored detector exceeded the feature bound\".into()); }\n    }",
        ),
        ArtifactKind::Mutator => (
            "use artifact::InstalledMacro;",
            "let artifacts = SmbCampaignArtifacts { generated_mutator: Some(SmbCampaignGeneratedMutator { name: ARTIFACT_NAME.to_owned(), source_sha256: SOURCE_SHA256.to_owned(), scope, mutator: &InstalledMacro }), generated_detector: None };",
            "for entry in &sample {\n        for seed in [0_u64, 0x5eed_dc00, u64::MAX] {\n            let a = InstalledMacro.mutate(&entry.input, seed);\n            let b = InstalledMacro.mutate(&entry.input, seed);\n            if a != b { return Err(\"authored macro was nondeterministic\".into()); }\n            if a.actions.len() > header.action_limit || !a.actions.starts_with(&entry.input.actions) || a.actions.iter().any(|chord| chord.hold_frames == 0 || chord.hold_frames > MAX_HOLD_FRAMES) { return Err(\"authored macro violated deterministic bounds\".into()); }\n        }\n    }",
        ),
    };
    format!(
        r####"// SPDX-License-Identifier: AGPL-3.0-or-later

mod artifact;

use std::{{error::Error, fs, io::BufWriter, path::{{Path, PathBuf}}}};

{artifact_use}
#[allow(unused_imports)]
use fuzzer::{{
    campaign::{{
        SmbCampaignArtifacts, SmbCampaignConfig, SmbCampaignGeneratedDetector,
        SmbCampaignGeneratedMutator, SmbCampaignOrigin, SmbCampaignStreamHeader,
        key_policy_from_identifier, replay_smb_campaign_with_artifacts,
        retention_from_identifier, run_smb_campaign_with_artifacts, scope_from_identifier,
        selector_from_identifier, vocabulary_from_identifier,
    }},
    phase4b::{{MAX_HOLD_FRAMES, SmbDetector, SmbMacro, observe_smb_input}},
    phase4c::SmbArchiveReport,
}};
use sha2::{{Digest, Sha256}};

const ARTIFACT_NAME: &str = "{artifact_name}";
const SOURCE_SHA256: &str = "{source_sha256}";
const SCOPE: &str = "{scope_identifier}";
const VERIFY_SAMPLE: usize = 4;

fn main() -> Result<(), Box<dyn Error>> {{
    let mut args = std::env::args_os().skip(1);
    let mode = args.next().ok_or("missing mode")?;
    match mode.to_str() {{
        Some("verify") => {{
            let stalled_run_dir = PathBuf::from(args.next().ok_or("missing stalled run directory")?);
            if args.next().is_some() {{ return Err("unexpected verify argument".into()); }}
            verify(&stalled_run_dir)
        }}
        Some("run") => {{
            let stalled_run_dir = PathBuf::from(args.next().ok_or("missing stalled run directory")?);
            let output = PathBuf::from(args.next().ok_or("missing output directory")?);
            let seed: u64 = args.next().ok_or("missing campaign seed")?.to_string_lossy().parse()?;
            let workers: u32 = args.next().ok_or("missing worker count")?.to_string_lossy().parse()?;
            let budget: u64 = args.next().ok_or("missing execution budget")?.to_string_lossy().parse()?;
            let host = args.next().ok_or("missing host name")?.to_string_lossy().into_owned();
            if args.next().is_some() {{ return Err("unexpected run argument".into()); }}
            run(&stalled_run_dir, &output, seed, workers, budget, host)
        }}
        Some("replay") => {{
            let run_dir = PathBuf::from(args.next().ok_or("missing attempt run directory")?);
            let stalled_run_dir = PathBuf::from(args.next().ok_or("missing stalled run directory")?);
            if args.next().is_some() {{ return Err("unexpected replay argument".into()); }}
            replay(&run_dir, &stalled_run_dir)
        }}
        _ => Err("unknown installed-campaign-artifact mode".into()),
    }}
}}

fn read_rom() -> Result<Vec<u8>, Box<dyn Error>> {{
    let rom_path = PathBuf::from(
        std::env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    Ok(fs::read(rom_path)?)
}}

fn read_header(stalled_run_dir: &Path) -> Result<SmbCampaignStreamHeader, Box<dyn Error>> {{
    let stream = fs::read_to_string(stalled_run_dir.join("stream.jsonl"))?;
    Ok(serde_json::from_str(
        stream.lines().next().ok_or("stalled stream is empty")?,
    )?)
}}

fn read_origin(stalled_run_dir: &Path) -> Result<(SmbCampaignOrigin, SmbArchiveReport), Box<dyn Error>> {{
    let archive_path = stalled_run_dir.join("archive-live.json");
    let bytes = fs::read(&archive_path)?;
    let report: SmbArchiveReport = serde_json::from_slice(&bytes)?;
    Ok((
        SmbCampaignOrigin::Archive {{
            path: archive_path.to_string_lossy().into_owned(),
            file_sha256: format!("{{:x}}", Sha256::digest(&bytes)),
            report: Box::new(report.clone()),
        }},
        report,
    ))
}}

/// Deterministic fixture sample: the stalled frontier pair's shortest
/// retained inputs.
fn frontier_sample(report: &SmbArchiveReport) -> Vec<fuzzer::phase4c::SmbArchiveEntryReport> {{
    let Some(pair) = report
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level))
        .max()
    else {{
        return Vec::new();
    }};
    let mut sample: Vec<_> = report
        .entries
        .iter()
        .filter(|entry| (entry.key.world, entry.key.level) == pair)
        .cloned()
        .collect();
    sample.sort_by_key(|entry| (entry.input.actions.len(), entry.id));
    sample.truncate(VERIFY_SAMPLE);
    sample
}}

fn verify(stalled_run_dir: &Path) -> Result<(), Box<dyn Error>> {{
    let rom = read_rom()?;
    let header = read_header(stalled_run_dir)?;
    let _ = &header;
    let (_, report) = read_origin(stalled_run_dir)?;
    let sample = frontier_sample(&report);
    if sample.is_empty() {{ return Err("stalled archive has no frontier entries to verify against".into()); }}
    {verify_artifact}
    println!("fixture_verified=true entries={{}}", sample.len());
    Ok(())
}}

fn run(
    stalled_run_dir: &Path,
    output: &Path,
    seed: u64,
    workers: u32,
    budget: u64,
    host: String,
) -> Result<(), Box<dyn Error>> {{
    let rom = read_rom()?;
    let header = read_header(stalled_run_dir)?;
    let (origin, _) = read_origin(stalled_run_dir)?;
    let scope = scope_from_identifier(SCOPE)?;
    {artifact_binding}
    let config = SmbCampaignConfig {{
        campaign_seed: seed,
        workers,
        execution_budget: budget,
        action_limit: header.action_limit,
        host,
        wall_budget: None,
        selector_policy: selector_from_identifier(&header.parent_scheduler)?,
        retention_policy: retention_from_identifier(&header.retention_policy)?,
        archive_entry_limit: header.archive_entry_limit,
        vocabulary: vocabulary_from_identifier(&header.controller_vocabulary)?,
        key_policy: key_policy_from_identifier(&header.key_policy)?,
    }};
    fs::create_dir_all(output)?;
    let stream_file = fs::File::create(output.join("stream.jsonl"))?;
    let mut stream = BufWriter::new(stream_file);
    let report = run_smb_campaign_with_artifacts(&rom, &config, &origin, &mut stream, &artifacts)?;
    drop(stream);
    fs::write(output.join("archive-live.json"), serde_json::to_vec_pretty(&report.archive)?)?;
    fs::write(output.join("campaign-report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!(
        "executions={{}} retained={{}} watermark=({{}},{{}},{{}})",
        report.executions_completed,
        report.archive.retained,
        report.archive.progress_watermark.world,
        report.archive.progress_watermark.level,
        report.archive.progress_watermark.progress,
    );
    Ok(())
}}

fn replay(run_dir: &Path, stalled_run_dir: &Path) -> Result<(), Box<dyn Error>> {{
    let rom = read_rom()?;
    let (_, source) = read_origin(stalled_run_dir)?;
    let scope = scope_from_identifier(SCOPE)?;
    {artifact_binding}
    let stream_bytes = fs::read(run_dir.join("stream.jsonl"))?;
    let report = replay_smb_campaign_with_artifacts(&rom, &stream_bytes, Some(&source), &artifacts)?;
    fs::write(run_dir.join("archive-replay.json"), serde_json::to_vec_pretty(&report.archive)?)?;
    fs::write(run_dir.join("campaign-report-replay.json"), serde_json::to_vec_pretty(&report)?)?;
    let live_archive = fs::read(run_dir.join("archive-live.json"))?;
    let replay_archive = fs::read(run_dir.join("archive-replay.json"))?;
    let live_report = fs::read(run_dir.join("campaign-report.json"))?;
    let replay_report = fs::read(run_dir.join("campaign-report-replay.json"))?;
    if live_archive != replay_archive || live_report != replay_report {{
        return Err("installed campaign replay diverged from the recorded run".into());
    }}
    println!("replay_verified=true");
    Ok(())
}}
"####
    )
}

fn bounded_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(BUILD_ERROR_LIMIT)
        .collect()
}

fn required_path(
    args: &mut impl Iterator<Item = OsString>,
    name: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    Ok(PathBuf::from(
        args.next().ok_or_else(|| format!("missing {name}"))?,
    ))
}

fn required_string(
    args: &mut impl Iterator<Item = OsString>,
    name: &str,
) -> Result<String, Box<dyn Error>> {
    Ok(args
        .next()
        .ok_or_else(|| format!("missing {name}"))?
        .to_string_lossy()
        .into_owned())
}

fn reject_extra_args(args: &mut impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    Ok(())
}

fn parse_u64(value: &str) -> Result<u64, Box<dyn Error>> {
    let normalized = value.replace('_', "");
    if let Some(hex) = normalized.strip_prefix("0x") {
        Ok(u64::from_str_radix(hex, 16)?)
    } else {
        Ok(normalized.parse()?)
    }
}

#[cfg(test)]
mod tests {
    use super::{ArtifactKind, installed_main_source, validate_decision};
    use fuzzer::{
        phase4a::{InstrumentorAction, InstrumentorDecision},
        phase4c::SmbArtifactScope,
    };

    fn decision(
        action: InstrumentorAction,
        source: &str,
        scope: Option<&str>,
    ) -> InstrumentorDecision {
        InstrumentorDecision {
            action,
            name: "suggested".to_owned(),
            rust_source: source.to_owned(),
            scope_to_lineage: None,
            scope: scope.map(str::to_owned),
            rationale: "fixture".to_owned(),
            strategy_journal: Default::default(),
        }
    }

    const DETECTOR_SOURCE: &str = "pub struct InstalledDetector;\nimpl fuzzer::phase4b::SmbDetector for InstalledDetector { fn features(&self, observations: &[fuzzer::phase4b::SmbObservations]) -> Vec<u64> { observations.iter().map(|o| u64::from(o.decoded.player_y_bucket)).collect() } }\n";
    const MACRO_SOURCE: &str = "pub struct InstalledMacro;\nimpl fuzzer::phase4b::SmbMacro for InstalledMacro { fn mutate(&self, input: &fuzzer::phase4b::SmbInput, seed: u64) -> fuzzer::phase4b::SmbInput { let mut out = input.clone(); out.actions.push(fuzzer::phase4b::ButtonChord::new(0x81, 2 + (seed % 10) as u8)); out } }\n";

    #[test]
    fn validation_accepts_the_exact_facades_with_a_scope() {
        let (kind, scope) = validate_decision(&decision(
            InstrumentorAction::InstallDetector,
            DETECTOR_SOURCE,
            Some("6,3,60,73"),
        ))
        .expect("detector decision validates");
        assert_eq!(kind, ArtifactKind::Detector);
        assert!(scope.contains(6, 3, 73));
        let (kind, _) = validate_decision(&decision(
            InstrumentorAction::InstallMutator,
            MACRO_SOURCE,
            Some("6,3,60,73"),
        ))
        .expect("macro decision validates");
        assert_eq!(kind, ArtifactKind::Mutator);
    }

    #[test]
    fn validation_requires_a_declared_scope() {
        assert!(
            validate_decision(&decision(
                InstrumentorAction::InstallDetector,
                DETECTOR_SOURCE,
                None,
            ))
            .is_err()
        );
        let mut lineage = decision(
            InstrumentorAction::InstallDetector,
            DETECTOR_SOURCE,
            Some("6,3,60,73"),
        );
        lineage.scope_to_lineage = Some(1);
        assert!(validate_decision(&lineage).is_err());
    }

    #[test]
    fn validation_rejects_forbidden_surfaces_and_wrong_facades() {
        for forbidden in [
            "pub struct InstalledDetector; impl fuzzer::phase4b::SmbDetector for InstalledDetector { fn features(&self, _: &[fuzzer::phase4b::SmbObservations]) -> Vec<u64> { while true {} } }",
            "pub struct InstalledDetector; impl fuzzer::phase4b::SmbDetector for InstalledDetector { fn features(&self, _: &[fuzzer::phase4b::SmbObservations]) -> Vec<u64> { std::fs::read(\"x\"); Vec::new() } }",
            "pub struct InstalledDetector; impl fuzzer::phase4b::SmbDetector for InstalledDetector { fn features(&self, _: &[fuzzer::phase4b::SmbObservations]) -> Vec<u64> { panic!() } }",
        ] {
            assert!(
                validate_decision(&decision(
                    InstrumentorAction::InstallDetector,
                    forbidden,
                    Some("6,3,60,73"),
                ))
                .is_err()
            );
        }
        // A macro source cannot install as a detector.
        assert!(
            validate_decision(&decision(
                InstrumentorAction::InstallDetector,
                MACRO_SOURCE,
                Some("6,3,60,73"),
            ))
            .is_err()
        );
        // Rankings are not a campaign install.
        assert!(
            validate_decision(&decision(
                InstrumentorAction::InstallRanking,
                DETECTOR_SOURCE,
                Some("6,3,60,73"),
            ))
            .is_err()
        );
    }

    #[test]
    fn generated_main_carries_the_artifact_provenance() {
        let scope = SmbArtifactScope {
            world: 6,
            level: 3,
            progress: (60, 73),
        };
        for kind in [ArtifactKind::Detector, ArtifactKind::Mutator] {
            let source = installed_main_source(kind, "stall_attempt_1_x", "abc123", scope);
            assert!(source.contains("const ARTIFACT_NAME: &str = \"stall_attempt_1_x\";"));
            assert!(source.contains("const SOURCE_SHA256: &str = \"abc123\";"));
            assert!(source.contains("const SCOPE: &str = \"6,3,60,73\";"));
            assert!(source.contains("run_smb_campaign_with_artifacts"));
            assert!(source.contains("replay_smb_campaign_with_artifacts"));
        }
    }
}
