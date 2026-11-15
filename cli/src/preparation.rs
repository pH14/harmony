// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    config::{Backend, Result},
    handoff::{ProfileFile, Record},
    package::{Operation, Request},
};
use std::{fs, path::Path};

pub fn execute(mut request: Request) -> Result<u8> {
    crate::host_boundary::probe_host()?;
    let mut artifacts = None;
    let mut rom = None;
    if request.selection.is_none() {
        match request.config.workload.package.as_str() {
            "faults" => {
                let mut config =
                    crate::adapters::faults::config::Config::from_shared(&request.config)?;
                if config.build.language.is_some()
                    || config.build.dockerfile.is_some()
                    || !config.build.command.is_empty()
                {
                    crate::adapters::faults::prepare::run(&mut config, request.offline)?;
                }
                crate::adapters::faults::config::resolve(&mut config, request.offline)?;
                if request.operation == Operation::Run && !request.command.is_empty() {
                    config.command = request.command.clone();
                }
                if request.operation == Operation::Run
                    && !config.command.is_empty()
                    && (!config.nodes.is_empty() || request.actions.is_some())
                {
                    return Err("command overrides cannot be combined with supervised scenarios; set nodes.NAME.command in TOML".into());
                }
                let (kernel, base) = crate::runtime::read_pair(&config.runtime())?;
                let staging = tempfile::tempdir()?;
                let image = oci_support::image::stage(
                    config.image.as_deref().ok_or("application image missing")?,
                    staging.path(),
                )?;
                let supervised = request.operation == Operation::Search
                    || request.actions.is_some()
                    || !config.nodes.is_empty();
                let launch = if supervised {
                    if let Some(bundle) = config.bundle()? {
                        oci_support::LaunchRequest::default()
                            .with_bundle("/etc/harmony/cli.bundle")
                            .with_external_inputs(vec![oci_support::ExternalInput::new(
                                "/etc/harmony/cli.bundle",
                                bundle.into_bytes(),
                            )])
                    } else {
                        oci_support::LaunchRequest::default()
                            .with_bundle(faults_workload::prepare::SUPERVISOR_BUNDLE)
                    }
                } else {
                    oci_support::LaunchRequest::new(config.command.clone())
                };
                let execution = if supervised {
                    oci_support::bundle::prepare(&image, &launch)?
                } else {
                    crate::adapters::faults::workflow::prepare_plain_staged(
                        &image,
                        &launch,
                        if config.backend == Backend::Uml {
                            faults_workload::admission::ExecutionMode::Uml
                        } else {
                            faults_workload::admission::ExecutionMode::Vm
                        },
                    )?
                };
                config.build = Default::default();
                request.config = config.shared()?;
                artifacts = Some((kernel, base, execution.record()));
            }
            "nes" => {
                let bytes = fs::read(
                    request
                        .config
                        .workload
                        .input
                        .as_ref()
                        .ok_or("provide a workload input")?,
                )?;
                nes_workload::package::RomKind::identify(&bytes)?;
                crate::runners::resolve(&mut request.config.runner, request.offline)?;
                let options = &request.config.workload.options;
                let image = options
                    .get("guest_image")
                    .and_then(toml::Value::as_str)
                    .ok_or("set workload.options.guest_image for the NES guest adapter")?;
                let execution = nes_workload::prepare::stage_and_prepare(image, &bytes)?;
                let runtime = crate::runners::consonance(&request.config.runner)?;
                let (kernel, base) = crate::runtime::read_pair(&runtime)?;
                artifacts = Some((kernel, base, execution.record()));
                rom = Some(bytes);
            }
            "nested" => {
                let runtime = crate::runners::consonance(&request.config.runner)?;
                if runtime.kernel.is_none() || runtime.base_initramfs.is_none() {
                    return Err("nested requires explicit kernel and base_initramfs".into());
                }
                crate::runners::resolve(&mut request.config.runner, request.offline)?;
                let runtime = crate::runners::consonance(&request.config.runner)?;
                let (kernel, base) = crate::runtime::read_pair(&runtime)?;
                let staging = tempfile::tempdir()?;
                let image = oci_support::image::stage(
                    request
                        .config
                        .workload
                        .input
                        .as_deref()
                        .ok_or("provide the nested driver image")?,
                    staging.path(),
                )?;
                let launch = oci_support::LaunchRequest::new(vec![
                    "/app/nested-driver".into(),
                    "--sdk".into(),
                    "--search".into(),
                ])
                .with_kvm();
                let execution = oci_support::bundle::prepare(&image, &launch)?;
                artifacts = Some((kernel, base, execution.record()));
            }
            _ => return Err("unknown workload package".into()),
        }
    }
    let mut record = Record::new(&request)?;
    if let Some((kernel, base, execution)) = artifacts {
        record.kernel = kernel;
        record.base = base;
        record.execution = Some(execution);
        record.rom = rom;
        let runtime = crate::runners::consonance(&request.config.runner)?;
        if runtime.runtime_qualification.is_some() {
            record.runtime_manifest = Some(fs::read(
                runtime
                    .base_initramfs
                    .as_ref()
                    .ok_or("missing base initramfs")?
                    .with_file_name("runtime-manifest.json"),
            )?);
        }
        if runtime.backend == Backend::Uml {
            collect_profile(
                runtime
                    .uml_profile
                    .as_deref()
                    .ok_or("missing UML profile")?,
                Path::new(""),
                &mut record.profile,
            )?;
        }
    }
    for (name, path) in [
        ("actions", &request.actions),
        ("exec-file", &request.exec_file),
    ] {
        if let Some(path) = path {
            record.inputs.insert(name.into(), fs::read(path)?);
        }
    }
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("request");
    record.write(&path)?;
    let file = fs::File::open(&path)?;
    fs::remove_file(&path)?;
    drop(directory);
    crate::host_boundary::enter_prepared_host(file, request.operation == Operation::Explain)
}

pub(crate) fn collect_profile(
    root: &Path,
    relative: &Path,
    files: &mut Vec<ProfileFile>,
) -> Result<()> {
    let mut entries = fs::read_dir(root.join(relative))?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect_profile(root, &path, files)?;
        } else if kind.is_file() {
            use std::os::unix::fs::PermissionsExt;
            files.push(ProfileFile {
                path,
                mode: entry.metadata()?.permissions().mode() & 0o777,
                bytes: fs::read(entry.path())?,
            });
        } else {
            return Err("UML profiles must contain ordinary files and directories".into());
        }
        if files.len() > 4096 {
            return Err("UML profile exceeds its file limit".into());
        }
    }
    Ok(())
}
