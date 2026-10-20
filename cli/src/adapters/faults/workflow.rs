// SPDX-License-Identifier: AGPL-3.0-or-later

use super::{
    config::{Backend, Config, Result},
    runs::Manifest,
};
use crate::runs::Destination;
#[cfg(feature = "frontend")]
use faults_workload::admission::ExecutionMode;
use faults_workload::{
    Artifacts, FaultVocabulary,
    package::{self, SearchStart},
};
#[cfg(feature = "frontend")]
use std::path::PathBuf;
use std::{fs, path::Path};

#[cfg(feature = "frontend")]
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

#[cfg(feature = "frontend")]
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
    store_runtime_artifacts(
        &mut manifest.record,
        path,
        artifacts,
        manifest.config.uml_profile.as_deref(),
    )?;
    manifest.save(path)
}

fn store_runtime_artifacts(
    record: &mut crate::runs::Manifest,
    path: &Path,
    artifacts: &Artifacts,
    uml_profile: Option<&Path>,
) -> Result<()> {
    record.store(path, "kernel", &artifacts.kernel)?;
    record.store(path, "initramfs", &artifacts.initramfs)?;
    if let Some(profile) = uml_profile {
        crate::runs::store_tree(record, path, profile, "uml")?;
    }
    Ok(())
}

#[cfg(feature = "frontend")]
fn prepare_faults(c: &Config) -> Result<(Artifacts, String, FaultVocabulary)> {
    let (kernel, base) = crate::runtime::read_pair(&c.runtime())?;
    let generated = c.bundle()?;
    let prepared = faults_workload::prepare::prepare_oci_with_bundle(
        c.image.as_deref().ok_or("application image missing")?,
        &base,
        generated.as_deref(),
        super::execution_mode(c.backend),
    )?;
    Ok((
        Artifacts {
            kernel,
            initramfs: prepared.initramfs,
        },
        prepared.bundle,
        prepared.vocabulary,
    ))
}

#[cfg(feature = "frontend")]
pub(crate) fn prepare_plain_staged(
    staged: &oci_support::image::StagedImage,
    request: &oci_support::bundle::LaunchRequest,
    mode: ExecutionMode,
) -> Result<oci_support::bundle::PreparedExecution> {
    prepare_plain_staged_with(staged, request, mode, || {})
}

#[cfg(feature = "frontend")]
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

#[cfg(feature = "frontend")]
pub fn search(c: Config, destination: Destination, offline: bool) -> Result<u8> {
    let c = configure(c, offline)?;
    let (artifacts, bundle, vocabulary) = prepare_faults(&c)?;
    search_prepared(c, destination, artifacts, bundle, vocabulary)
}

pub fn search_prepared(
    c: Config,
    destination: Destination,
    artifacts: Artifacts,
    bundle: String,
    vocabulary: FaultVocabulary,
) -> Result<u8> {
    let vocabulary = vocabulary.excluding(&c.exclude_faults);
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

#[cfg(feature = "frontend")]
pub fn continue_search(request: crate::adapters::Request) -> Result<u8> {
    let saved = crate::runs::Saved::read(
        &request
            .selection
            .as_ref()
            .ok_or("select a search or branch")?
            .run,
    )?;
    continue_saved(request, saved)
}

pub fn continue_saved(request: crate::adapters::Request, saved: crate::runs::Saved) -> Result<u8> {
    let selection = request.selection.ok_or("select a search or branch")?;
    let parent = saved.path;
    let original = Manifest::from_record(saved.manifest)?;
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
                    crate::config::UNBOUNDED_EXECUTIONS - completed
                } else {
                    crate::config::DEFAULT_EXECUTIONS
                });
        shared.search.executions = Some(
            completed
                .checked_add(additional)
                .ok_or("execution budget overflow")?,
        );
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
        "{} executions; {} confirmed findings; {} unmet reachability assertions; {} hung jobs",
        report.executions,
        report.bugs.iter().filter(|b| b.confirmed).count(),
        report.never_satisfied.len(),
        report.watchdog_cutoffs
    );
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
#[cfg(feature = "frontend")]
pub fn run(
    config: Config,
    destination: Destination,
    actions: Option<PathBuf>,
    repeat: u32,
    console: bool,
    offline: bool,
    command: Vec<String>,
) -> Result<u8> {
    crate::preparation::execute(crate::adapters::Request {
        operation: crate::adapters::Operation::Run,
        config: config.shared()?,
        destination,
        actions,
        repeat,
        console,
        offline,
        command,
        ..Default::default()
    })
}

pub fn run_prepared(
    c: Config,
    request: crate::adapters::Request,
    prepared: crate::handoff::Prepared,
) -> Result<u8> {
    let crate::adapters::Request {
        destination,
        actions,
        repeat,
        console,
        ..
    } = request;
    let execution = prepared
        .execution
        .as_ref()
        .ok_or("missing prepared execution")?;
    let artifacts = Artifacts {
        kernel: prepared.kernel.clone(),
        initramfs: execution.initramfs(&prepared.base),
    };
    if let Some(bundle) = prepared.bundle {
        let vocabulary = prepared
            .vocabulary
            .as_ref()
            .ok_or("missing admitted vocabulary")?;
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

#[cfg(all(test, feature = "frontend"))]
mod tests {
    use super::*;

    fn elf(code: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0; 4097 + code.len()];
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
        for (offset, value) in [
            (72, 4096_u64),
            (80, 0x1000),
            (96, code.len() as u64),
            (104, code.len() as u64),
            (112, 4096),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[120..124].copy_from_slice(&0x6474e551_u32.to_le_bytes());
        bytes[124..128].copy_from_slice(&6_u32.to_le_bytes());
        bytes[260..264].copy_from_slice(&1_u32.to_le_bytes());
        for (offset, value) in [
            (264, 6_u64),
            (272, 0x1000),
            (280, 4096),
            (288, code.len() as u64),
            (304, 1),
        ] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[324..328].copy_from_slice(&3_u32.to_le_bytes());
        for (offset, value) in [(344, 4096 + code.len() as u64), (352, 1), (368, 1)] {
            bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        bytes[4096..4096 + code.len()].copy_from_slice(code);
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
    fn ordinary_uninstrumented_uml_preparation_refuses_transaction_entry() {
        for (code, name) in [
            (&[0xc7, 0xf8, 0, 0, 0, 0, 0xc3][..], "XBEGIN"),
            (&[0xf2, 0x87, 0, 0xc3][..], "XACQUIRE"),
        ] {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir(root.path().join("bin")).unwrap();
            let path = root.path().join("bin/app");
            fs::write(&path, elf(code)).unwrap();
            let staged = oci_support::image::StagedImage {
                rootfs: root.path().to_path_buf(),
                owners: oci_support::image::Ownership::default(),
                config: oci_support::image::RuntimeConfig::default(),
            };
            let request = oci_support::bundle::LaunchRequest::default();
            let before = fs::read(&path).unwrap();
            let vm = prepare_plain_staged(&staged, &request, ExecutionMode::Vm).unwrap();
            let report =
                faults_workload::admission::ArchivedImage::new(&vm.archived_rootfs().unwrap())
                    .inspect()
                    .unwrap();
            assert!(!report.instrumented_events());
            let error = prepare_plain_staged(&staged, &request, ExecutionMode::Uml)
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains(name), "{error}");
            assert_eq!(fs::read(&path).unwrap(), before);
            fs::write(&path, elf(&[0xf3, 0xa4, 0x90, 0xc3])).unwrap();
            assert!(prepare_plain_staged(&staged, &request, ExecutionMode::Uml).is_ok());
        }
    }

    #[test]
    fn plain_vm_and_uml_preparation_enforce_xstate_policy_without_instrumentation() {
        for (code, expected) in [
            (&[0x0f, 0xae, 0x07, 0xc3][..], Some("save instruction")),
            (&[0x0f, 0xae, 0x27, 0xc3][..], Some("save instruction")),
            (
                &[0xb9, 1, 0, 0, 0, 0x0f, 0x01, 0xd0, 0xc3][..],
                Some("XGETBV"),
            ),
            (&[0x31, 0xc9, 0x0f, 0x01, 0xd0, 0xc3][..], None),
            (&[0x0f, 0xae, 0x2f, 0xc3][..], None),
            (&[0xd9, 0xfe, 0x0f, 0x52, 0xc0, 0xc3][..], None),
        ] {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir(root.path().join("bin")).unwrap();
            let path = root.path().join("bin/app");
            let bytes = elf(code);
            fs::write(&path, &bytes).unwrap();
            let staged = oci_support::image::StagedImage {
                rootfs: root.path().to_path_buf(),
                owners: oci_support::image::Ownership::default(),
                config: oci_support::image::RuntimeConfig::default(),
            };
            for mode in [ExecutionMode::Vm, ExecutionMode::Uml] {
                let prepared = prepare_plain_staged(
                    &staged,
                    &oci_support::bundle::LaunchRequest::default(),
                    mode,
                );
                match expected {
                    Some(message) => assert!(prepared.err().unwrap().to_string().contains(message)),
                    None => {
                        let prepared = prepared.unwrap();
                        let archive = prepared.archived_rootfs().unwrap();
                        let report = faults_workload::admission::ArchivedImage::new(&archive)
                            .inspect()
                            .unwrap();
                        assert!(!report.instrumented_events());
                        assert!(report.require_for(mode).is_ok());
                    }
                }
            }
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[ignore = "requires cc and GNU objdump"]
    fn native_xstate_preparation_matches_stronger_checker() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("bin"))?;
        let scratch = tempfile::tempdir()?;
        let binary = root.path().join("bin/app");
        let source = scratch.path().join("probe.S");
        let evidence = std::env::var_os("HARMONY_XSTATE_ADMISSION_REPORT_DIR").map(PathBuf::from);
        if let Some(path) = &evidence {
            fs::create_dir(path)?;
        }
        let staged = oci_support::image::StagedImage {
            rootfs: root.path().to_path_buf(),
            owners: oci_support::image::Ownership::default(),
            config: oci_support::image::RuntimeConfig::default(),
        };
        for (name, assembly, admitted) in [
            ("fxsave", "fxsave (%rdi)", false),
            ("xsave", "xsave (%rdi)", false),
            ("selector1", "mov $1,%ecx; xgetbv", false),
            ("selector0", "xor %ecx,%ecx; xgetbv", true),
            ("restore", "xrstor (%rdi)", true),
            ("math", "fsin; rsqrtps %xmm0,%xmm0", true),
            ("standard-libc", "int main(void) { return 0; }", true),
        ] {
            let mut compiler = std::process::Command::new("cc");
            compiler.args(["-static", "-no-pie", "-Wl,--build-id=none"]);
            if name == "standard-libc" {
                fs::write(&source, assembly)?;
                compiler.args(["-x", "c", "-O2"]);
            } else {
                fs::write(
                    &source,
                    format!(
                        ".text\n.globl _start\n_start:\n{assembly}\nret\n.section .note.GNU-stack,\"\",@progbits\n"
                    ),
                )?;
                compiler.args(["-nostdlib", "-Wl,-e,_start"]);
            }
            assert!(
                compiler
                    .arg(&source)
                    .arg("-o")
                    .arg(&binary)
                    .status()?
                    .success()
            );
            let bytes = fs::read(&binary)?;
            if let Some(path) = &evidence {
                fs::write(path.join(format!("{name}.elf")), &bytes)?;
            }
            for (mode, label) in [(ExecutionMode::Vm, "vm"), (ExecutionMode::Uml, "uml")] {
                if name == "standard-libc" && mode == ExecutionMode::Uml {
                    continue;
                }
                let output = scratch.path().join(format!("{name}-{label}.json"));
                let python = std::process::Command::new("python3")
                    .arg(
                        Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join("../consonance/harmony-linux/scripts/x86-xstate-admission.py"),
                    )
                    .arg("verify")
                    .arg(root.path())
                    .args(["--execution-mode", label, "--output"])
                    .arg(&output)
                    .status()?;
                let audit = fs::read(&output)?;
                let json: serde_json::Value = serde_json::from_slice(&audit)?;
                assert_eq!(python.success(), admitted, "{name} {label}: {json}");
                let expected_error = match name {
                    "fxsave" | "xsave" => Some("save instruction outside a loader trampoline"),
                    "selector1" => Some("unproved XGETBV selector"),
                    _ => None,
                };
                if let Some(message) = expected_error {
                    assert!(
                        json["errors"].as_array().unwrap().iter().any(|error| error
                            .as_str()
                            .is_some_and(|error| error.contains(message))),
                        "{name} {label}: {json}"
                    );
                }
                let result = prepare_plain_staged(
                    &staged,
                    &oci_support::bundle::LaunchRequest::default(),
                    mode,
                );
                assert_eq!(result.is_ok(), admitted, "{name} {label}");
                if let Some(message) = expected_error {
                    assert!(
                        result.as_ref().err().unwrap().to_string().contains(message),
                        "{name} {label}"
                    );
                }
                if let Some(path) = &evidence {
                    fs::write(path.join(format!("{name}-{label}-python.json")), &audit)?;
                    fs::write(
                        path.join(format!("{name}-{label}-rust.txt")),
                        match result {
                            Ok(_) => "admitted".into(),
                            Err(e) => e.to_string(),
                        },
                    )?;
                }
                assert_eq!(fs::read(&binary)?, bytes);
            }
            println!(
                "XSTATE_ADMISSION={name}:{}",
                if admitted { "admitted" } else { "refused" }
            );
        }
        Ok(())
    }

    fn prepared_fixture_request(
        execution: &oci_support::PreparedExecution,
        config: &Config,
    ) -> Result<crate::package::Request> {
        Ok(crate::package::Request {
            operation: if execution.execution().bundle.is_some() {
                crate::package::Operation::Search
            } else {
                crate::package::Operation::Run
            },
            config: config.shared()?,
            ..Default::default()
        })
    }

    #[test]
    fn prepared_fixture_declares_supervised_intent_before_handoff_admission() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::create_dir_all(root.path().join("etc/harmony"))?;
        fs::write(
            root.path().join("etc/harmony/bundle"),
            b"node app /bin/app\n",
        )?;
        let image = oci_support::image::StagedImage {
            rootfs: root.path().to_owned(),
            owners: Default::default(),
            config: Default::default(),
        };
        for supervised in [false, true] {
            let launch = if supervised {
                oci_support::LaunchRequest::default()
                    .with_bundle(faults_workload::prepare::SUPERVISOR_BUNDLE)
            } else {
                oci_support::LaunchRequest::new(vec!["/bin/app".into()])
            };
            let execution = oci_support::bundle::prepare(&image, &launch)?;
            let config = Config {
                backend: Backend::Kvm,
                command: if supervised {
                    Vec::new()
                } else {
                    execution.execution().argv.clone()
                },
                ..Default::default()
            };
            let request = prepared_fixture_request(&execution, &config)?;
            let mut record = crate::handoff::Record::new(&request)?;
            record.kernel = b"captured kernel".to_vec();
            record.base = b"captured base".to_vec();
            record.execution = Some(execution.record());
            let file = tempfile::NamedTempFile::new()?;
            record.write(file.path())?;
            let (request, prepared) = crate::handoff::Record::read(file.path())?;
            assert_eq!(
                request.operation,
                if supervised {
                    crate::package::Operation::Search
                } else {
                    crate::package::Operation::Run
                }
            );
            assert_eq!(prepared.bundle.is_some(), supervised);
        }
        Ok(())
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn prepared_uml_handoff(
        execution: &oci_support::PreparedExecution,
        profile_root: &Path,
    ) -> Result<(crate::package::Request, crate::handoff::Prepared)> {
        let profile = uml::Profile::load(profile_root)?;
        let kernel = profile.directory().join(&profile.profile().executable.name);
        let base = profile.directory().join(&profile.profile().rootfs.name);
        let config = Config {
            image: Some("captured-image".into()),
            backend: Backend::Uml,
            kernel: Some(kernel.clone()),
            base_initramfs: Some(base.clone()),
            uml_profile: Some(profile_root.to_path_buf()),
            command: if execution.execution().bundle.is_none() {
                execution.execution().argv.clone()
            } else {
                Vec::new()
            },
            ..Default::default()
        };
        let request = prepared_fixture_request(execution, &config)?;
        let mut record = crate::handoff::Record::new(&request)?;
        record.kernel = fs::read(kernel)?;
        record.base = fs::read(base)?;
        record.execution = Some(execution.record());
        crate::preparation::collect_profile(profile_root, Path::new(""), &mut record.profile)?;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("prepared");
        record.write(&path)?;
        crate::handoff::Record::read(&path)
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    #[test]
    #[ignore = "requires cc and a reviewed x86 UML profile"]
    fn native_uml_transaction_entry_is_refused_before_launch() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("bin"))?;
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../workloads/faults/tests/fixtures/uml_transaction_init.c");
        let staged = oci_support::image::StagedImage {
            rootfs: root.path().to_path_buf(),
            owners: oci_support::image::Ownership::default(),
            config: oci_support::image::RuntimeConfig::default(),
        };
        let request = oci_support::bundle::LaunchRequest::default();
        let binary = root.path().join("bin/app");
        let audits = tempfile::tempdir()?;
        let evidence = std::env::var_os("HARMONY_UML_TRANSACTION_REPORT_DIR").map(PathBuf::from);
        if let Some(directory) = &evidence {
            fs::create_dir(directory)?;
        }
        for (kind, expected) in [(1, Some("XBEGIN")), (2, Some("XACQUIRE")), (0, None)] {
            let status = std::process::Command::new("cc")
                .args([
                    "-O2",
                    "-static",
                    "-nostdlib",
                    "-ffreestanding",
                    "-fno-stack-protector",
                    "-fno-pie",
                    "-no-pie",
                    "-Wl,-e,_start",
                    "-Wl,--build-id=none",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                ])
                .arg(format!("-DTRANSACTION_KIND={kind}"))
                .arg(&source)
                .arg("-o")
                .arg(&binary)
                .status()?;
            assert!(status.success());
            let bytes = fs::read(&binary)?;
            assert!(bytes.starts_with(b"\x7fELF"));
            if let Some(directory) = &evidence {
                fs::write(directory.join(format!("kind-{kind}.elf")), &bytes)?;
            }
            let vm = prepare_plain_staged(&staged, &request, ExecutionMode::Vm)?;
            let archive = vm.archived_rootfs()?;
            let inspected = faults_workload::admission::ArchivedImage::new(&archive).inspect()?;
            assert!(!inspected.instrumented_events());
            assert!(inspected.scan_passed());
            let profile_root = PathBuf::from(std::env::var("HARMONY_UML_REVIEWED_PROFILE")?);
            fs::create_dir_all(root.path().join("etc/harmony"))?;
            fs::write(
                root.path().join(faults_workload::prepare::BUNDLE_PATH),
                b"node app /bin/app\n",
            )?;
            let supervised = oci_support::bundle::prepare(
                &staged,
                &oci_support::LaunchRequest::default()
                    .with_bundle(faults_workload::prepare::SUPERVISOR_BUNDLE),
            )?;
            for execution in [&vm, &supervised] {
                let imported = prepared_uml_handoff(execution, &profile_root);
                if let Some(name) = expected {
                    let error = imported
                        .err()
                        .ok_or("prepared UML admitted native transaction entry")?
                        .to_string();
                    assert!(error.contains(name), "{error}");
                } else {
                    imported?;
                }
            }
            if let Some(name) = expected {
                println!("UML_PREPARED_TRANSACTION_REFUSED={name}");
            } else {
                println!("UML_PREPARED_TRANSACTION_CONTROL=admitted");
            }
            let scanner = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../consonance/harmony-linux/scripts/x86-xstate-admission.py");
            let output = audits.path().join(format!("{kind}.json"));
            let python = std::process::Command::new("python3")
                .arg(scanner)
                .arg("verify")
                .arg(root.path())
                .args(["--execution-mode", "uml", "--output"])
                .arg(&output)
                .status()?;
            let audit_bytes = fs::read(output)?;
            if let Some(directory) = &evidence {
                fs::write(
                    directory.join(format!("kind-{kind}-python.json")),
                    &audit_bytes,
                )?;
            }
            let audit: serde_json::Value = serde_json::from_slice(&audit_bytes)?;
            assert_eq!(audit["execution_mode"], "uml");
            assert_eq!(python.success(), expected.is_none(), "{audit}");
            if expected.is_some() {
                assert!(
                    audit["errors"][0]
                        .as_str()
                        .unwrap()
                        .contains("native transaction entry")
                );
            }
            if let Some(name) = expected {
                let error = prepare_plain_staged(&staged, &request, ExecutionMode::Uml)
                    .err()
                    .ok_or("native transaction entry was admitted")?
                    .to_string();
                if let Some(directory) = &evidence {
                    fs::write(directory.join(format!("kind-{kind}-rust.txt")), &error)?;
                }
                assert!(error.contains(name), "{error}");
                assert_eq!(fs::read(&binary)?, bytes);
                println!("UML_TRANSACTION_REFUSED={name}");
            } else {
                prepare_plain_staged(&staged, &request, ExecutionMode::Uml)?;
                let artifacts = tempfile::tempdir()?;
                let initramfs = artifacts.path().join("initramfs.cpio.gz");
                let packer = Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../consonance/uml/tests/fixtures/pack_fsgsbase_initramfs.py");
                assert!(
                    std::process::Command::new("python3")
                        .arg(packer)
                        .arg(&binary)
                        .arg(&initramfs)
                        .status()?
                        .success()
                );
                let directory = std::env::var("HARMONY_UML_REVIEWED_PROFILE")?;
                let profile = uml::Profile::load(Path::new(&directory))?;
                let work = artifacts.path().join("work");
                fs::create_dir(&work)?;
                let mut launch = uml::Launch::new(work);
                launch.memory_mib = 128;
                launch.wall_limit = std::time::Duration::from_secs(30);
                launch.console_tail_bytes = 64 << 10;
                launch.set_initramfs(&initramfs)?;
                let exit = uml::Guest::spawn(&launch, &profile)?.wait()?;
                if let Some(report) = &evidence {
                    fs::write(report.join("control-console.txt"), &exit.console)?;
                    fs::write(
                        report.join("profile.json"),
                        fs::read(Path::new(&directory).join("profile.json"))?,
                    )?;
                }
                let console = String::from_utf8_lossy(&exit.console);
                assert_eq!(exit.reason, uml::ExitReason::Exited(0), "{console}");
                assert!(
                    console.contains("UML TRANSACTION CONTROL PASS"),
                    "{console}"
                );
                assert!(exit.leftovers.is_empty());
                assert!(exit.work_removed);
                println!("UML_TRANSACTION_CONTROL=completed");
            }
        }
        Ok(())
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
        let mut config = crate::config::Config::default();
        config.runner.kind = "test".into();
        let mut record = crate::runs::Manifest::new(config, "run").unwrap();

        store_runtime_artifacts(&mut record, directory.path(), &artifacts, None).unwrap();
        record.save(directory.path()).unwrap();

        assert_eq!(
            fs::read(directory.path().join("artifacts/kernel")).unwrap(),
            artifacts.kernel
        );
        assert_eq!(
            record.artifacts["kernel"],
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
