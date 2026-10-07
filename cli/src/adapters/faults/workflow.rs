// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{
    config::{Backend, Config, Result},
    runs::Manifest,
};
use crate::runs::Destination;
use faults_workload::{
    Artifacts, FaultVocabulary,
    package::{self, SearchStart},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn check(mut c: Config, offline: bool, json: bool) -> Result<u8> {
    let runtime_error = super::config::resolve(&mut c, offline)
        .err()
        .map(|error| error.to_string());
    let admission = c
        .image
        .as_deref()
        .map(faults_workload::admission::inspect_image)
        .transpose()?;
    let ready = runtime_error.is_none()
        && admission
            .as_ref()
            .is_none_or(|r| r.scan_passed() && r.attestation_errors.is_empty());
    let report = serde_json::json!({ "os": std::env::consts::OS, "architecture": std::env::consts::ARCH,
        "blockers": runtime_error.iter().collect::<Vec<_>>(), "backend": c.backend, "kernel": c.kernel, "base_initramfs": c.base_initramfs,
        "uml_profile": c.uml_profile, "admission": admission, "ready": ready, "bundle": c.bundle()? });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "backend: {:?}\nruntime: {}",
            c.backend,
            runtime_error.as_deref().unwrap_or("ready")
        );
        if let Some(r) = admission {
            println!(
                "image admission: {}\ninstrumented event faults: {}",
                r.scan_passed(),
                r.instrumented_events()
            );
            for error in r.scan_errors.iter().chain(&r.attestation_errors) {
                println!("  {error}");
            }
            for file in r.instrumented {
                println!("  instrumented: {}", file.path);
            }
        }
        println!("next: harmony search");
    }
    Ok(if ready { 0 } else { 2 })
}

fn configure(mut c: Config, offline: bool) -> Result<Config> {
    if c.image.is_none() {
        return Err("provide a workload input or configure its build".into());
    }
    if c.build.language.is_some() || c.build.dockerfile.is_some() || !c.build.command.is_empty() {
        super::prepare::run(&mut c, offline)?;
    }
    super::config::resolve(&mut c, offline)?;
    Ok(c)
}

fn capture_runtime(manifest: &mut Manifest, path: &Path, initramfs: &[u8]) -> Result<()> {
    let kernel = manifest.config.kernel.as_ref().ok_or("missing kernel")?;
    let kernel = fs::read(kernel)?;
    manifest.store(path, "kernel", &kernel)?;
    manifest.store(path, "initramfs", initramfs)?;
    if let Some(profile) = manifest.config.uml_profile.clone() {
        crate::runs::store_tree(&mut manifest.record, path, &profile, "uml")?;
    }
    manifest.save(path)
}

fn prepare_faults(c: &Config) -> Result<(Artifacts, String, FaultVocabulary)> {
    let base = fs::read(c.base_initramfs.as_ref().ok_or("missing base initramfs")?)?;
    let generated = c.bundle()?;
    let prepared = faults_workload::prepare::prepare_oci_with_bundle(
        c.image.as_deref().ok_or("application image missing")?,
        &base,
        generated.as_deref(),
    )?;
    Ok((
        Artifacts {
            kernel: fs::read(c.kernel.as_ref().ok_or("missing kernel")?)?,
            initramfs: prepared.initramfs,
        },
        prepared.bundle,
        prepared.vocabulary,
    ))
}

pub fn finish(path: &Path, manifest: &mut Manifest, result: Result<u8>) -> Result<u8> {
    match &result {
        Ok(_) => manifest.status = "complete".into(),
        Err(error) => {
            manifest.status = "failed".into();
            manifest.error = Some(error.to_string());
        }
    }
    manifest.save(path)?;
    println!(
        "{}: {}\nshow: harmony show {}",
        manifest.mode,
        path.display(),
        path.display()
    );
    result
}

pub fn report_code(report: &faults_workload::Report) -> u8 {
    if report.execution_failures > 0 || report.watchdog_cutoffs > 0 {
        2
    } else if report.bug_found || !report.never_satisfied.is_empty() {
        1
    } else {
        0
    }
}

pub fn search(c: Config, destination: Destination, offline: bool) -> Result<u8> {
    let c = configure(c, offline)?;
    let (artifacts, bundle, vocabulary) = prepare_faults(&c)?;
    let out = destination.create()?;
    let mut manifest = Manifest::new(c, "search", Some(bundle))?;
    capture_runtime(&mut manifest, &out, &artifacts.initramfs)?;
    manifest.store(&out, "vocabulary.json", &serde_json::to_vec(&vocabulary)?)?;
    manifest.save(&out)?;
    let result = search_faults(&out, &manifest, &SearchStart::Genesis);
    finish(&out, &mut manifest, result)
}

pub fn continue_search(request: crate::adapters::Request) -> Result<u8> {
    let selection = request.selection.ok_or("select a search or branch")?;
    let parent = crate::runs::locate(&selection.run)?;
    let original = Manifest::read(&parent)?;
    original.verify(&parent)?;
    if original.bundle.is_none() {
        return Err("this execution has no searchable supervisor scenario".into());
    }
    let resume = request.operation == crate::adapters::Operation::Resume;
    let mut shared = original.config.shared()?;
    request.budget.apply(&mut shared)?;
    let start = if resume {
        if original.mode != "search" {
            return Err(
                "search --resume requires a saved search; use search --from for a branch".into(),
            );
        }
        let (checkpoint, completed) = crate::runs::checkpoint_record(&parent)?;
        let additional =
            request
                .budget
                .executions
                .unwrap_or(if request.budget.wall_seconds.is_some() {
                    i64::MAX as u64 - completed
                } else {
                    1000
                });
        shared.search.executions = completed
            .checked_add(additional)
            .ok_or("execution budget overflow")?;
        shared.search.wall_seconds = request.budget.wall_seconds;
        shared.validate()?;
        SearchStart::Checkpoint(checkpoint)
    } else {
        SearchStart::Actions(super::investigate::prefix(
            &parent,
            &original,
            &selection,
            &request.point,
        )?)
    };
    let config = Config::from_shared(&shared)?;
    let out = request.destination.create()?;
    let mut manifest = original.inherit(&parent, &out, "search")?;
    manifest.config = config;
    manifest.actions.clear();
    manifest.settle = true;
    manifest.save(&out)?;
    let result = search_faults(&out, &manifest, &start);
    finish(&out, &mut manifest, result)
}

fn search_faults(out: &Path, manifest: &Manifest, start: &SearchStart) -> Result<u8> {
    let artifacts = manifest.fault_artifacts(out)?;
    let vocabulary: FaultVocabulary =
        serde_json::from_slice(&fs::read(out.join("artifacts/vocabulary.json"))?)?;
    let options = manifest.options(out);
    let resources = package::resources(&options)?;
    let worker = if manifest.config.backend == Backend::Uml {
        None
    } else {
        Some(consonance_client::session::WorkerLauncher::current_exe(
            vec!["session-worker".into()],
        )?)
    };
    let report = package::search(&artifacts, &vocabulary, &options, &resources, worker, start)?;
    println!(
        "{} executions; {} confirmed findings; {} unmet reachability assertions",
        report.executions,
        report.bugs.iter().filter(|b| b.confirmed).count(),
        report.never_satisfied.len()
    );
    Ok(report_code(&report))
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    config: Config,
    destination: Destination,
    actions: Option<PathBuf>,
    repeat: u32,
    console: bool,
    offline: bool,
    command: Vec<String>,
) -> Result<u8> {
    let mut c = configure(config, offline)?;
    if !command.is_empty() {
        c.command = command;
    }
    if !c.command.is_empty() && (!c.nodes.is_empty() || actions.is_some()) {
        return Err("command overrides cannot be combined with supervised scenarios; set nodes.NAME.command in TOML".into());
    }
    if actions.is_some() || !c.nodes.is_empty() {
        let (artifacts, bundle, vocabulary) = prepare_faults(&c)?;
        let actions = match actions {
            Some(path) => {
                faults_workload::parse_recorded_input(&fs::read_to_string(path)?)?.actions
            }
            None => vec![faults_workload::FaultAction::new(
                faults_workload::FaultOperation::Wait(std::num::NonZeroU16::new(100).unwrap()),
                std::num::NonZeroU16::new(256).unwrap(),
            )],
        };
        let out = destination.create()?;
        let mut manifest = Manifest::new(c, "run", Some(bundle))?;
        manifest.actions = actions;
        capture_runtime(&mut manifest, &out, &artifacts.initramfs)?;
        manifest.store(&out, "vocabulary.json", &serde_json::to_vec(&vocabulary)?)?;
        manifest.save(&out)?;
        let result = package::replay(
            &artifacts,
            &manifest.actions,
            repeat,
            &manifest.options(&out),
        )
        .map(|report| report_code(&report));
        return finish(&out, &mut manifest, result);
    }
    if repeat != 1 {
        return Err(
            "--repeat requires an action scenario; use replay for recorded commands".into(),
        );
    }
    let stage = tempfile::tempdir()?;
    let staged =
        oci_support::image::stage(c.image.as_deref().ok_or("missing image")?, stage.path())?;
    faults_workload::admission::inspect_root(&staged.rootfs)?.require_admission()?;
    let prepared = oci_support::bundle::prepare(
        &staged,
        &oci_support::bundle::LaunchRequest::new(c.command.clone()),
    )?;
    let initramfs =
        prepared.initramfs(&fs::read(c.base_initramfs.as_ref().ok_or("missing base")?)?);
    let out = destination.create()?;
    let mut manifest = Manifest::new(c, "run", None)?;
    capture_runtime(&mut manifest, &out, &initramfs)?;
    let kernel = fs::read(out.join("artifacts/kernel"))?;
    let result = crate::oci::execute(
        &manifest.config.runtime(),
        manifest.config.seed,
        &manifest.config.knobs,
        manifest.config.wall_seconds,
        &kernel,
        &initramfs,
        &out,
        console,
    )
    .map(|passed| if passed { 0 } else { 1 });
    finish(&out, &mut manifest, result)
}
