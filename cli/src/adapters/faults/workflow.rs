// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{
    config::{Backend, Config, Result},
    runs::Manifest,
};
use crate::runs::Destination;
use faults_workload::{
    Artifacts, FaultVocabulary,
    admission::ExecutionMode,
    package::{self, SearchStart},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub fn check(mut c: Config, offline: bool, json: bool) -> Result<u8> {
    let requested_mode = super::requested_execution_mode(&c);
    let runtime_error = super::config::resolve(&mut c, offline)
        .err()
        .map(|error| error.to_string());
    let admission = c
        .image
        .as_deref()
        .map(faults_workload::admission::inspect_image)
        .transpose()?;
    let admission_error = admission
        .as_ref()
        .and_then(|report| {
            requested_mode
                .as_ref()
                .ok()
                .and_then(|mode| report.require_for(*mode).err())
        })
        .map(|error| error.to_string());
    let ready = runtime_error.is_none() && requested_mode.is_ok() && admission_error.is_none();
    let blockers = runtime_error
        .iter()
        .chain(admission_error.iter())
        .collect::<Vec<_>>();
    let report = serde_json::json!({ "os": std::env::consts::OS, "architecture": std::env::consts::ARCH,
        "blockers": blockers, "backend": c.backend, "kernel": c.kernel, "base_initramfs": c.base_initramfs,
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
                admission_error.is_none() && requested_mode.is_ok(),
                r.instrumented_events()
            );
            for error in r.scan_errors.iter().chain(&r.attestation_errors) {
                println!("  {error}");
            }
            if let Some(error) = &admission_error {
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

fn capture_runtime(manifest: &mut Manifest, path: &Path, artifacts: &Artifacts) -> Result<()> {
    manifest.store(path, "kernel", &artifacts.kernel)?;
    manifest.store(path, "initramfs", &artifacts.initramfs)?;
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
        super::execution_mode(c.backend),
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

fn prepare_plain_staged(
    staged: &oci_support::image::StagedImage,
    request: &oci_support::bundle::LaunchRequest,
    mode: ExecutionMode,
) -> Result<oci_support::bundle::PreparedExecution> {
    prepare_plain_staged_with(staged, request, mode, || {})
}

fn prepare_plain_staged_with(
    staged: &oci_support::image::StagedImage,
    request: &oci_support::bundle::LaunchRequest,
    mode: ExecutionMode,
    after_archive: impl FnOnce(),
) -> Result<oci_support::bundle::PreparedExecution> {
    let prepared = oci_support::bundle::prepare(staged, request)?;
    after_archive();
    faults_workload::admission::ArchivedImage::new(&prepared.archived_rootfs()?)
        .inspect()?
        .require_for(mode)?;
    Ok(prepared)
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
    let mut manifest = Manifest::new(c, "search", Some(bundle), true)?;
    capture_runtime(&mut manifest, &out, &artifacts)?;
    manifest.store(&out, "vocabulary.json", &serde_json::to_vec(&vocabulary)?)?;
    manifest.save(&out)?;
    let result = search_faults(&out, &manifest, &SearchStart::Genesis).and_then(|report| {
        manifest.pin_outcome(&report)?;
        Ok(report_code(&report))
    });
    finish(&out, &mut manifest, result)
}

pub fn continue_search(request: crate::adapters::Request) -> Result<u8> {
    let selection = request.selection.ok_or("select a search or branch")?;
    let parent = crate::runs::locate(&selection.run)?;
    let original = Manifest::read(&parent)?;
    let resume = request.operation == crate::adapters::Operation::Resume;
    if resume {
        original.record.verify(&parent)?;
    } else {
        original.verify(&parent)?;
    }
    if original.bundle.is_none() {
        return Err("this execution has no searchable supervisor scenario".into());
    }
    let mut shared = original.config.shared()?;
    request.budget.apply(&mut shared)?;
    let start = if resume {
        if original.mode != "search" {
            return Err(
                "search --resume requires a saved search; use search --from for a branch".into(),
            );
        }
        let (checkpoint, completed) =
            crate::runs::checkpoint_record_verified(&parent, &original.record)?;
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
    let result = search_faults(&out, &manifest, &start).and_then(|report| {
        manifest.pin_outcome(&report)?;
        Ok(report_code(&report))
    });
    finish(&out, &mut manifest, result)
}

fn read_vocabulary(out: &Path, manifest: &Manifest) -> Result<FaultVocabulary> {
    let bytes = fs::read(out.join("artifacts/vocabulary.json"))?;
    let expected = manifest
        .artifacts
        .get("vocabulary.json")
        .ok_or("recorded vocabulary.json artifact is missing")?;
    if crate::runtime::digest(&bytes) != *expected {
        return Err("recorded artifact vocabulary.json changed during load".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn search_faults(
    out: &Path,
    manifest: &Manifest,
    start: &SearchStart,
) -> Result<faults_workload::Report> {
    let artifacts = manifest.fault_artifacts(out)?;
    let vocabulary = read_vocabulary(out, manifest)?;
    let options = manifest.options(out)?;
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
    Ok(report)
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
        let mut manifest = Manifest::new(c, "run", Some(bundle), true)?;
        manifest.actions = actions;
        capture_runtime(&mut manifest, &out, &artifacts)?;
        manifest.store(&out, "vocabulary.json", &serde_json::to_vec(&vocabulary)?)?;
        manifest.save(&out)?;
        let result = manifest
            .options(&out)
            .and_then(|options| package::replay(&artifacts, &manifest.actions, repeat, &options))
            .and_then(|report| {
                manifest.pin_outcome(&report)?;
                Ok(report_code(&report))
            });
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
    let prepared = prepare_plain_staged(
        &staged,
        &oci_support::bundle::LaunchRequest::new(c.command.clone()),
        super::execution_mode(c.backend),
    )?;
    let initramfs =
        prepared.initramfs(&fs::read(c.base_initramfs.as_ref().ok_or("missing base")?)?);
    let artifacts = Artifacts {
        kernel: fs::read(c.kernel.as_ref().ok_or("missing kernel")?)?,
        initramfs,
    };
    let out = destination.create()?;
    let mut manifest = Manifest::new(c, "run", None, true)?;
    capture_runtime(&mut manifest, &out, &artifacts)?;
    let result = (|| -> Result<u8> {
        let profile_identity = manifest.uml_profile_identity(&out)?;
        let (passed, record) = crate::oci::execute(
            &manifest.config.runtime(),
            &manifest.runner_identity,
            profile_identity.as_deref(),
            manifest.config.seed,
            &manifest.config.knobs,
            manifest.config.wall_seconds,
            &artifacts.kernel,
            &artifacts.initramfs,
            &out,
            console,
        )?;
        manifest.pin_outcome(&record)?;
        Ok(if passed { 0 } else { 1 })
    })();
    finish(&out, &mut manifest, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf(code: &[u8; 4]) -> Vec<u8> {
        let mut bytes = vec![0; 4101];
        bytes[..16].copy_from_slice(b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00");
        for (offset, value) in [
            (16, 2_u16),
            (18, 62),
            (52, 64),
            (54, 56),
            (56, 2),
            (58, 64),
            (60, 3),
            (62, 2),
        ] {
            bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        bytes[20..24].copy_from_slice(&1_u32.to_le_bytes());
        for (offset, value) in [(24, 0x1000_u64), (32, 64), (40, 192)] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[64..68].copy_from_slice(&1_u32.to_le_bytes());
        bytes[68..72].copy_from_slice(&5_u32.to_le_bytes());
        for (offset, value) in [(72, 4096_u64), (80, 0x1000), (96, 4), (104, 4), (112, 4096)] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[120..124].copy_from_slice(&0x6474e551_u32.to_le_bytes());
        bytes[124..128].copy_from_slice(&6_u32.to_le_bytes());
        bytes[260..264].copy_from_slice(&1_u32.to_le_bytes());
        for (offset, value) in [(264, 6_u64), (272, 0x1000), (280, 4096), (288, 4), (304, 1)] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[324..328].copy_from_slice(&3_u32.to_le_bytes());
        for (offset, value) in [(344, 4100_u64), (352, 1), (368, 1)] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[4096..4100].copy_from_slice(code);
        bytes
    }

    #[test]
    fn plain_run_admits_the_executable_that_was_packed() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("bin")).unwrap();
        let path = root.path().join("bin/app");
        fs::write(&path, elf(&[0x90, 0x90, 0x90, 0xc3])).unwrap();
        let staged = oci_support::image::StagedImage {
            rootfs: root.path().to_path_buf(),
            owners: oci_support::image::Ownership::default(),
            config: oci_support::image::RuntimeConfig::default(),
        };
        let request = oci_support::bundle::LaunchRequest::default();
        let reference = oci_support::bundle::prepare(&staged, &request).unwrap();
        let expected = reference.rootfs_segment();
        let prepared = prepare_plain_staged_with(&staged, &request, ExecutionMode::Uml, || {
            fs::write(&path, elf(&[0x0f, 0xc7, 0xf0, 0xc3])).unwrap()
        })
        .unwrap();
        assert_eq!(prepared.rootfs_segment(), expected);
        assert!(
            faults_workload::admission::inspect_root(root.path())
                .unwrap()
                .require_for(ExecutionMode::Uml)
                .is_err()
        );
    }

    #[test]
    fn search_refuses_vocabulary_changed_after_initial_verification() {
        let directory = tempfile::tempdir().unwrap();
        let mut shared = crate::config::Config::default();
        shared.runner.kind = "test".into();
        shared.workload.package = "faults".into();
        let mut record = crate::runs::Manifest::new(shared, "search").unwrap();
        let saved = FaultVocabulary::new(1, vec![1, 2]).unwrap();
        record
            .store(
                directory.path(),
                "vocabulary.json",
                &serde_json::to_vec(&saved).unwrap(),
            )
            .unwrap();
        record.payload = serde_json::json!({
            "bundle":"node app \"/bin/app\"\n","actions":[],"settle":true,"outcome":null
        });
        record.save(directory.path()).unwrap();
        let manifest = Manifest::read(directory.path()).unwrap();
        manifest.record.verify(directory.path()).unwrap();
        let substituted = FaultVocabulary::new(1, vec![]).unwrap();
        fs::write(
            directory.path().join("artifacts/vocabulary.json"),
            serde_json::to_vec(&substituted).unwrap(),
        )
        .unwrap();

        assert!(
            read_vocabulary(directory.path(), &manifest)
                .unwrap_err()
                .to_string()
                .contains("vocabulary.json changed during load")
        );

        fs::write(
            directory.path().join("artifacts/vocabulary.json"),
            serde_json::to_vec(&saved).unwrap(),
        )
        .unwrap();
        assert_eq!(read_vocabulary(directory.path(), &manifest).unwrap(), saved);
    }

    #[test]
    fn captured_kernel_is_the_prepared_kernel_after_source_changes() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("kernel-source");
        fs::write(&source, b"executed kernel").unwrap();
        let artifacts = Artifacts {
            kernel: fs::read(&source).unwrap(),
            initramfs: b"prepared initramfs".to_vec(),
        };
        fs::write(&source, b"later kernel").unwrap();
        let config = Config {
            backend: Backend::Kvm,
            kernel: Some(source),
            ..Config::default()
        };
        let mut manifest = Manifest::new(config, "run", None, true).unwrap();

        capture_runtime(&mut manifest, directory.path(), &artifacts).unwrap();

        assert_eq!(
            fs::read(directory.path().join("artifacts/kernel")).unwrap(),
            artifacts.kernel
        );
        assert_eq!(
            manifest.artifacts["kernel"],
            crate::runtime::digest(&artifacts.kernel)
        );
    }

    #[test]
    fn completed_search_refuses_report_replaced_after_package_return() {
        let directory = tempfile::tempdir().unwrap();
        let mut shared = crate::config::Config::default();
        shared.runner.kind = "test".into();
        shared.workload.package = "faults".into();
        let mut record = crate::runs::Manifest::new(shared, "search").unwrap();
        record.store(directory.path(), "kernel", b"kernel").unwrap();
        record
            .store(directory.path(), "initramfs", b"image")
            .unwrap();
        record.payload = serde_json::json!({
            "bundle":"node app \"/bin/app\"\n","actions":[],"settle":true,"outcome":null
        });
        record.save(directory.path()).unwrap();
        let mut manifest = Manifest::read(directory.path()).unwrap();
        let returned: faults_workload::Report = serde_json::from_value(serde_json::json!({
            "package":"faults","mode":"search","image_sha256":crate::runtime::digest(b"image"),
            "kernel_sha256":crate::runtime::digest(b"kernel"),"identity":"runner","seed":0,"workers":1,
            "ram_mib":1024,"executions":1,"bug_found":false,
            "first_bug_execution":null,"bugs":[],"never_satisfied":[],"replays":[],
            "execution_ticks":1,"wall_seconds":1,"execution_failures":0
        }))
        .unwrap();
        assert_eq!(report_code(&returned), 0);
        returned.write(directory.path()).unwrap();
        manifest.pin_outcome(&returned).unwrap();
        let mut replaced = returned.clone();
        replaced
            .never_satisfied
            .push("fabricated reachability failure".into());
        replaced.write(directory.path()).unwrap();

        assert!(finish(directory.path(), &mut manifest, Ok(report_code(&returned))).is_err());
        let saved = Manifest::read(directory.path()).unwrap();
        assert!(
            saved
                .verify(directory.path())
                .unwrap_err()
                .to_string()
                .contains("report.json has changed")
        );

        let mut replaced = returned.clone();
        replaced.executions += 1;
        assert_eq!(report_code(&replaced), report_code(&returned));
        replaced.write(directory.path()).unwrap();
        assert!(finish(directory.path(), &mut manifest, Ok(report_code(&returned))).is_err());

        fs::remove_file(directory.path().join("report.json")).unwrap();
        assert!(finish(directory.path(), &mut manifest, Ok(report_code(&returned))).is_err());

        returned.write(directory.path()).unwrap();
        assert_eq!(
            finish(directory.path(), &mut manifest, Ok(report_code(&returned))).unwrap(),
            0
        );
        let saved = Manifest::read(directory.path()).unwrap();
        assert_eq!(saved.status, "complete");
        assert_eq!(
            saved.record.payload["outcome"],
            serde_json::to_value(returned).unwrap()
        );
    }

    #[test]
    fn completed_command_run_requires_the_producer_record() {
        let directory = tempfile::tempdir().unwrap();
        let mut shared = crate::config::Config::default();
        shared.runner.kind = "test".into();
        shared.workload.package = "faults".into();
        let mut record = crate::runs::Manifest::new(shared, "run").unwrap();
        record.payload = serde_json::json!({
            "bundle":null,"actions":[],"settle":true,"outcome":null
        });
        record.save(directory.path()).unwrap();
        let mut manifest = Manifest::read(directory.path()).unwrap();
        let produced = serde_json::json!({"container_rc":1,"serial_sha256":"original"});
        fs::write(
            directory.path().join("run.json"),
            serde_json::to_vec(&produced).unwrap(),
        )
        .unwrap();
        let replaced = serde_json::json!({"container_rc":0,"serial_sha256":"replaced"});
        fs::write(
            directory.path().join("run.json"),
            serde_json::to_vec(&replaced).unwrap(),
        )
        .unwrap();

        assert!(finish(directory.path(), &mut manifest, Ok(1)).is_err());
        manifest.pin_outcome(&produced).unwrap();
        assert!(finish(directory.path(), &mut manifest, Ok(1)).is_err());

        let same_code = serde_json::json!({"container_rc":1,"serial_sha256":"replaced"});
        fs::write(
            directory.path().join("run.json"),
            serde_json::to_vec(&same_code).unwrap(),
        )
        .unwrap();
        assert!(finish(directory.path(), &mut manifest, Ok(1)).is_err());

        fs::write(
            directory.path().join("run.json"),
            serde_json::to_vec(&produced).unwrap(),
        )
        .unwrap();
        assert_eq!(finish(directory.path(), &mut manifest, Ok(1)).unwrap(), 1);
        let saved = Manifest::read(directory.path()).unwrap();
        assert_eq!(saved.status, "complete");
        assert_eq!(saved.record.payload["outcome"], produced);
    }
}
