// SPDX-License-Identifier: AGPL-3.0-or-later

//! Host-side preparation, validation, install, restart, and replay for M2.

use std::{
    env,
    error::Error,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use fuzzer::phase4a::{
    AdventureRunReport, InstalledAdventureDetectorReport, InstrumentorAction, InstrumentorDecision,
    InstrumentorRequest, prove_adventure_base_plateau,
};
use serde::Serialize;

const GENERATED_SOURCE_LIMIT: usize = 262_144;
const BUILD_ERROR_LIMIT: usize = 16_384;
const INSTALLED_REPORT: &str = "phase4a-installed-detector-report.json";

#[derive(Debug, Serialize)]
struct InstallReport {
    decision: InstrumentorDecision,
    host_name: String,
    fixture_verified: bool,
    campaign: InstalledAdventureDetectorReport,
    replay_verified: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let command = args.next().ok_or("missing subcommand")?;
    match command.to_str() {
        Some("prepare") => prepare(args),
        Some("request") => request(args),
        Some("install") => install(args),
        _ => Err("unknown model-instrumentor subcommand".into()),
    }
}

fn prepare(mut args: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let plateau_report = required_path(&mut args, "plateau report")?;
    let trial_dir = required_path(&mut args, "trial directory")?;
    reject_extra_args(args)?;
    fs::create_dir_all(&trial_dir)?;
    let operator_view = trial_dir.join("operator-view");
    fs::create_dir(&operator_view)?;
    fs::create_dir(operator_view.join("corpus"))?;

    let plateau: AdventureRunReport = serde_json::from_slice(&fs::read(&plateau_report)?)?;
    let proof = prove_adventure_base_plateau(&plateau)?;
    if proof.child_can_add_base_novelty || proof.child_can_reach_target {
        return Err("M2 input is not a closed base-map plateau".into());
    }
    if plateau.triage_events.len() != plateau.corpus.len() {
        return Err("M2 plateau labels do not cover the complete corpus".into());
    }

    fs::write(
        operator_view.join("fuzzer_stats"),
        format!(
            "target : adventure-toy\nexecs_done : {}\ncorpus_count : {}\nmaximum_progress : {}\ntarget_reached : false\nplateau_proven : true\n",
            plateau.executions,
            plateau.corpus.len(),
            plateau.maximum_progress,
        ),
    )?;
    fs::write(
        operator_view.join("plateau-proof.json"),
        serde_json::to_vec_pretty(&proof)?,
    )?;
    fs::write(
        operator_view.join("plot_data"),
        format!(
            "# deterministic campaign: no wall-clock columns\nexecs_done,corpus_count,maximum_progress\n{},{},{}\n",
            plateau.executions,
            plateau.corpus.len(),
            plateau.maximum_progress,
        ),
    )?;
    fs::write(
        operator_view.join("input-vocabulary.txt"),
        "Inputs are bounded ordered lists over seven total enum actions. One append mutation is attempted per target execution. Inapplicable actions are deterministic no-ops. Testcase action lists are intentionally not exposed to the instrumentor.\n",
    )?;
    fs::write(
        operator_view.join("observation-format.txt"),
        "Each action boundary reports room (Start, Key, Door, Treasure, or Hazard), has_key, door_open, target, and crashed. The base novelty map retains room identity only. Labels were produced by Luna from the same visible evidence. The numeric mechanical progress field is not an instrumentation oracle.\n",
    )?;
    fs::write(
        operator_view.join("detector-interface.txt"),
        "Generated source is the complete detector.rs file. It declares `pub struct InstalledDetector;` and implements `fuzzer::phase4a::AdventureDetector` with `fn features(&self, observations: &[fuzzer::target::AdventureObservations]) -> Vec<u64>`. Detector keys are global coverage bits: once any testcase emits a key, that key alone is no longer novel in another base state. The host indexes each returned key as key % 64, so keys with equal low six bits also collide. To preserve state distinctions hidden by the room-only base map across later mutations, encode relevant visible-field/base-state conjunctions with distinct low six bits rather than only standalone boolean milestones. The function is pure, deterministic, bounded, and receives the complete observation trace.\n",
    )?;
    for event in &plateau.triage_events {
        let stem = format!("testcase-{:020}", event.request.testcase_id);
        fs::write(
            operator_view.join("corpus").join(format!("{stem}.json")),
            serde_json::to_vec_pretty(&event.request)?,
        )?;
        fs::write(
            operator_view
                .join("corpus")
                .join(format!("{stem}.labels.json")),
            serde_json::to_vec_pretty(&event.labels)?,
        )?;
    }
    fs::write(
        trial_dir.join("plateau-report.path"),
        plateau_report.as_os_str().as_encoded_bytes(),
    )?;
    Ok(())
}

fn request(mut args: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let trial: u8 = required_string(&mut args, "trial")?.parse()?;
    let attempt: u8 = required_string(&mut args, "attempt")?.parse()?;
    let previous_error = args
        .next()
        .map(PathBuf::from)
        .map(fs::read_to_string)
        .transpose()?;
    reject_extra_args(args)?;
    serde_json::to_writer(
        std::io::stdout().lock(),
        &InstrumentorRequest {
            trial,
            attempt,
            previous_error,
        },
    )?;
    Ok(())
}

fn install(mut args: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    let plateau_report = required_path(&mut args, "plateau report")?;
    let trial_dir = required_path(&mut args, "trial directory")?;
    let decision_path = required_path(&mut args, "decision file")?;
    let seed: u64 = required_string(&mut args, "seed")?.parse()?;
    let execution_budget: u64 = required_string(&mut args, "execution budget")?.parse()?;
    reject_extra_args(args)?;

    let decision: InstrumentorDecision = serde_json::from_slice(&fs::read(&decision_path)?)?;
    validate_decision(&decision)?;
    let build_dir = trial_dir.join("build/installed-detector");
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
            "[package]\nname = \"m2-installed-detector\"\nversion = \"0.1.0\"\nedition = \"2024\"\nlicense = \"AGPL-3.0-or-later\"\n\n[dependencies]\nfuzzer = {{ path = \"{dependency_path}\" }}\n\n[workspace]\n"
        ),
    )?;
    let detector_source = if decision
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
    fs::write(source_dir.join("detector.rs"), detector_source)?;
    fs::write(source_dir.join("main.rs"), installed_main_source())?;

    let target_dir = trial_dir.join("build/target");
    let build = Command::new("cargo")
        .arg("build")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(build_dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        .env("CARGO_NET_OFFLINE", "true")
        .output()?;
    fs::write(trial_dir.join("build.stdout"), &build.stdout)?;
    fs::write(trial_dir.join("build.stderr"), &build.stderr)?;
    if !build.status.success() {
        return Err(format!(
            "generated detector build failed: {}",
            bounded_lossy(&build.stderr)
        )
        .into());
    }
    let binary = target_dir.join("debug").join(if cfg!(windows) {
        "m2-installed-detector.exe"
    } else {
        "m2-installed-detector"
    });

    let fixture = Command::new(&binary)
        .arg("verify")
        .arg(&plateau_report)
        .output()?;
    fs::write(trial_dir.join("fixture.stdout"), &fixture.stdout)?;
    fs::write(trial_dir.join("fixture.stderr"), &fixture.stderr)?;
    if !fixture.status.success() {
        return Err(format!(
            "generated detector fixture failed: {}",
            bounded_lossy(&fixture.stderr)
        )
        .into());
    }

    let installed_dir = trial_dir.join("installed-campaign");
    let replay_dir = trial_dir.join("installed-campaign-replay");
    let scope = decision
        .scope_to_lineage
        .map_or_else(|| "none".to_owned(), |value| value.to_string());
    run_installed(
        &binary,
        &plateau_report,
        &installed_dir,
        seed,
        execution_budget,
        &scope,
        &trial_dir.join("campaign"),
    )?;
    run_installed(
        &binary,
        &plateau_report,
        &replay_dir,
        seed,
        execution_budget,
        &scope,
        &trial_dir.join("replay"),
    )?;
    let campaign: InstalledAdventureDetectorReport =
        serde_json::from_slice(&fs::read(installed_dir.join(INSTALLED_REPORT))?)?;
    let replay: InstalledAdventureDetectorReport =
        serde_json::from_slice(&fs::read(replay_dir.join(INSTALLED_REPORT))?)?;
    let replay_verified = campaign == replay;
    let report = InstallReport {
        decision,
        host_name: "installed_detector".to_owned(),
        fixture_verified: true,
        campaign,
        replay_verified,
    };
    fs::write(
        trial_dir.join("model-instrumentor-install-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "target={:?} detector_novelties={} detector_active={} replay={}",
        report.campaign.time_to_target,
        report.campaign.detector.novelties,
        report.campaign.detector.active,
        report.replay_verified,
    );
    Ok(())
}

fn run_installed(
    binary: &Path,
    plateau_report: &Path,
    output_dir: &Path,
    seed: u64,
    execution_budget: u64,
    scope: &str,
    record_stem: &Path,
) -> Result<(), Box<dyn Error>> {
    let output = Command::new(binary)
        .arg("run")
        .arg(plateau_report)
        .arg(output_dir)
        .arg(seed.to_string())
        .arg(execution_budget.to_string())
        .arg(scope)
        .output()?;
    fs::write(record_stem.with_extension("stdout"), &output.stdout)?;
    fs::write(record_stem.with_extension("stderr"), &output.stderr)?;
    if !output.status.success() {
        return Err(format!(
            "installed detector campaign failed: {}",
            bounded_lossy(&output.stderr)
        )
        .into());
    }
    Ok(())
}

fn validate_decision(decision: &InstrumentorDecision) -> Result<(), Box<dyn Error>> {
    if decision.action != InstrumentorAction::InstallDetector {
        return Err(format!(
            "instrumentor returned unsupported action {:?}",
            decision.action
        )
        .into());
    }
    if decision.rust_source.is_empty() || decision.rust_source.len() > GENERATED_SOURCE_LIMIT {
        return Err("generated detector source is empty or exceeds 256 KiB".into());
    }
    if !decision
        .rust_source
        .contains("pub struct InstalledDetector")
        || !decision
            .rust_source
            .contains("AdventureDetector for InstalledDetector")
    {
        return Err("generated source does not implement the exact detector facade".into());
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
        "File::",
        "OpenOptions",
        "Tcp",
        "Udp",
    ] {
        if decision.rust_source.contains(forbidden) {
            return Err(format!("generated source contains forbidden token {forbidden:?}").into());
        }
    }
    Ok(())
}

fn installed_main_source() -> &'static str {
    "// SPDX-License-Identifier: AGPL-3.0-or-later\n\nmod detector;\n\nuse std::{error::Error, path::PathBuf};\nuse detector::InstalledDetector;\nuse fuzzer::phase4a::{run_installed_adventure_detector, verify_installed_adventure_detector};\n\nfn main() -> Result<(), Box<dyn Error>> {\n    let mut args = std::env::args_os().skip(1);\n    let mode = args.next().ok_or(\"missing mode\")?;\n    let plateau = PathBuf::from(args.next().ok_or(\"missing plateau report\")?);\n    match mode.to_str() {\n        Some(\"verify\") => {\n            if args.next().is_some() { return Err(\"unexpected verify argument\".into()); }\n            verify_installed_adventure_detector(&InstalledDetector, &plateau)\n        }\n        Some(\"run\") => {\n            let output = PathBuf::from(args.next().ok_or(\"missing output directory\")?);\n            let seed: u64 = args.next().ok_or(\"missing seed\")?.to_string_lossy().parse()?;\n            let budget: u64 = args.next().ok_or(\"missing budget\")?.to_string_lossy().parse()?;\n            let scope_arg = args.next().ok_or(\"missing lineage scope\")?;\n            if args.next().is_some() { return Err(\"unexpected run argument\".into()); }\n            let scope = if scope_arg == \"none\" { None } else { Some(scope_arg.to_string_lossy().parse()?) };\n            run_installed_adventure_detector(&plateau, &output, seed, budget, scope, InstalledDetector).map(|_| ())\n        }\n        _ => Err(\"unknown installed-detector mode\".into()),\n    }\n}\n"
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

fn reject_extra_args(mut args: impl Iterator<Item = OsString>) -> Result<(), Box<dyn Error>> {
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    Ok(())
}
