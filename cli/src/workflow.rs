// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    config::{Backend, Config, Result, Source},
    runs::{Destination, Manifest},
};
use faults_workload::{
    Artifacts, FaultVocabulary,
    package::{self, SearchStart},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn doctor(mut c: Config, offline: bool, json: bool) -> Result<u8> {
    let runtime_error = crate::runtime::resolve(&mut c, offline)
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

fn configure(source: Source, offline: bool) -> Result<Config> {
    let mut c = source.load()?;
    if c.image.is_none() && c.rom.is_none() {
        return Err(
            "provide an OCI image, a .nes file, or harmony.toml; start with harmony init".into(),
        );
    }
    if c.build.language.is_some() || c.build.dockerfile.is_some() || !c.build.command.is_empty() {
        crate::prepare::run(&mut c, offline)?;
    }
    crate::runtime::resolve(&mut c, offline)?;
    Ok(c)
}

fn capture_runtime(manifest: &mut Manifest, path: &Path, initramfs: &[u8]) -> Result<()> {
    let kernel = manifest.config.kernel.as_ref().ok_or("missing kernel")?;
    manifest.store(path, "kernel", &fs::read(kernel)?)?;
    manifest.store(path, "initramfs", initramfs)?;
    if let Some(profile) = manifest.config.uml_profile.clone() {
        crate::runs::store_tree(manifest, path, &profile, "uml")?;
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
        "run: {}\ninspect: harmony inspect {}",
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

pub fn search(
    source: Source,
    destination: Destination,
    from: Option<String>,
    resume: Option<String>,
    bug: Option<usize>,
    offline: bool,
) -> Result<u8> {
    if let Some(parent) = from.as_ref().or(resume.as_ref()) {
        if source.input.is_some()
            || source.config.is_some()
            || source.config_toml.is_some()
            || source.backend.is_some()
            || source.kernel.is_some()
            || source.base_initramfs.is_some()
            || source.uml_profile.is_some()
            || source.ram_mib.is_some()
            || !source.knobs.is_empty()
            || source.core.is_some()
            || source.nes_image.is_some()
        {
            return Err("search continuations inherit execution configuration; only seed, executions and --for may change".into());
        }
        let parent = crate::runs::locate(parent)?;
        let original = Manifest::read(&parent)?;
        if original.config.rom.is_some() {
            return Err("NES continuation uses the game campaign tools; this command currently continues application searches".into());
        }
        original.verify(&parent)?;
        let start = if resume.is_some() {
            SearchStart::Checkpoint(latest_checkpoint(&parent)?)
        } else {
            SearchStart::Actions(crate::investigate::selected_actions(
                &parent, &original, bug,
            )?)
        };
        let out = destination.create()?;
        let mut manifest = original.inherit(&parent, &out, "search")?;
        if let Some(v) = source.seed {
            manifest.config.seed = v;
        }
        if let Some(v) = source.executions {
            manifest.config.executions = v;
        }
        if let Some(v) = source.wall_seconds {
            manifest.config.wall_seconds = Some(v);
        }
        manifest.actions.clear();
        manifest.settle = true;
        manifest.save(&out)?;
        let result = search_faults(&out, &manifest, &start);
        return finish(&out, &mut manifest, result);
    }
    let c = configure(source, offline)?;
    if c.rom.is_some() {
        return search_nes(c, destination);
    }
    let (artifacts, bundle, vocabulary) = prepare_faults(&c)?;
    let out = destination.create()?;
    let mut manifest = Manifest::new(c, "search", Some(bundle))?;
    capture_runtime(&mut manifest, &out, &artifacts.initramfs)?;
    manifest.store(&out, "vocabulary.json", &serde_json::to_vec(&vocabulary)?)?;
    manifest.save(&out)?;
    let result = search_faults(&out, &manifest, &SearchStart::Genesis);
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

fn latest_checkpoint(path: &Path) -> Result<PathBuf> {
    let directory = path.join("checkpoints");
    let text = fs::read_to_string(directory.join("checkpoints.jsonl")).map_err(
        |_| "this run has no whole-search checkpoint yet (written periodically and when a search completes)",
    )?;
    for (index, line) in text.lines().rev().enumerate() {
        let record: serde_json::Value = match serde_json::from_str(line) {
            Ok(record) => record,
            Err(error) if index == 0 && !text.ends_with('\n') && error.is_eof() => continue,
            Err(error) => return Err(error.into()),
        };
        if let Some(file) = record["file"].as_str() {
            let candidate = directory.join(
                Path::new(file)
                    .file_name()
                    .ok_or("invalid checkpoint filename")?,
            );
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }
    Err("no retained search checkpoint exists".into())
}

fn search_nes(c: Config, destination: Destination) -> Result<u8> {
    let rom = fs::read(c.rom.as_ref().ok_or("missing ROM")?)?;
    let out = destination.create()?;
    let mut manifest = Manifest::new(c.clone(), "search", None)?;
    manifest.store(&out, "rom", &rom)?;
    let options = nes_workload::package::SearchOptions {
        seed: c.seed,
        workers: consonance_client::placement::CorePool::detect()?
            .max_workers()
            .try_into()?,
        executions: c.executions,
        output: out.join("nes"),
    };
    let result = if c.backend == Backend::Native {
        let core = c.core.as_ref().ok_or("missing NES core")?;
        manifest.store(&out, "core", &fs::read(core)?)?;
        nes_workload::package::search_native(&rom, &out.join("artifacts/core"), &options)
    } else {
        #[cfg(target_os = "linux")]
        {
            let prepared = nes_workload::prepare::stage_and_prepare(
                c.nes_image
                    .as_deref()
                    .ok_or("set nes_image for guest NES execution")?,
                &rom,
            )?;
            let base = fs::read(c.base_initramfs.as_ref().ok_or("missing base")?)?;
            capture_runtime(&mut manifest, &out, &prepared.initramfs(&base))?;
            nes_workload::package::search_consonance(
                &rom,
                &fs::read(c.kernel.as_ref().ok_or("missing kernel")?)?,
                &prepared,
                &base,
                &options,
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err("guest NES execution requires Linux KVM".into())
        }
    };
    if result.is_ok() {
        for entry in fs::read_dir(&options.output)? {
            let entry = entry?;
            fs::rename(entry.path(), out.join(entry.file_name()))?;
        }
        fs::remove_dir(&options.output)?;
    }
    finish(&out, &mut manifest, result.map(|_| 0))
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    source: Source,
    destination: Destination,
    actions: Option<PathBuf>,
    repeat: u32,
    console: bool,
    offline: bool,
    command: Vec<String>,
) -> Result<u8> {
    let mut c = configure(source, offline)?;
    if !command.is_empty() {
        c.command = command;
    }
    if c.rom.is_some() {
        return Err("use harmony search for NES inputs".into());
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
    let result = crate::oci::execute(&manifest.config, &kernel, &initramfs, &out, console)
        .map(|passed| if passed { 0 } else { 1 });
    finish(&out, &mut manifest, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_checkpoint_append_preserves_the_previous_checkpoint() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("checkpoints");
        fs::create_dir(&directory).unwrap();
        let checkpoint = directory.join("checkpoint.bin");
        fs::write(&checkpoint, b"checkpoint").unwrap();
        let journal = directory.join("checkpoints.jsonl");
        for suffix in ["", "{", "{\"file\":", "{\"file\":\"next"] {
            fs::write(
                &journal,
                format!("{{\"file\":\"checkpoint.bin\"}}\n{suffix}"),
            )
            .unwrap();
            assert_eq!(latest_checkpoint(root.path()).unwrap(), checkpoint);
        }
        fs::write(&journal, "{\"file\":\"checkpoint.bin\"}\n{\"file\":broken}").unwrap();
        assert!(latest_checkpoint(root.path()).is_err());
        fs::write(&journal, "{\"file\":\"checkpoint.bin\"}\n{\n").unwrap();
        assert!(latest_checkpoint(root.path()).is_err());
    }
}
