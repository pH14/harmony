// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    config::{Backend, Result},
    package::{Operation, Request},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[cfg(any(feature = "frontend", test))]
use std::io::{BufWriter, Write};

const FORMAT: &str = "harmony-prepared-execution-v1";
const LIMIT: u64 = 1 << 30;
const REQUEST_LIMIT: usize = 1 << 20;

#[derive(Deserialize, Serialize)]
pub struct Record {
    pub format: String,
    pub release: String,
    pub source: String,
    pub request: Vec<u8>,
    pub kernel: Vec<u8>,
    pub base: Vec<u8>,
    pub runtime_manifest: Option<Vec<u8>>,
    pub execution: Option<oci_support::bundle::PreparedExecutionRecord>,
    pub rom: Option<Vec<u8>>,
    pub inputs: BTreeMap<String, Vec<u8>>,
    pub profile: Vec<ProfileFile>,
}

#[derive(Deserialize, Serialize)]
pub struct ProfileFile {
    pub path: PathBuf,
    pub mode: u32,
    pub bytes: Vec<u8>,
}

pub struct Prepared {
    pub kernel: Vec<u8>,
    pub base: Vec<u8>,
    pub execution: Option<oci_support::PreparedExecution>,
    pub rom: Option<Vec<u8>>,
    pub bundle: Option<String>,
    pub vocabulary: Option<faults_workload::FaultVocabulary>,
    pub saved: Option<crate::runs::Saved>,
    _files: tempfile::TempDir,
}

impl Prepared {
    pub fn resolve_saved(&mut self, request: &mut Request) -> Result<()> {
        if let Some(selection) = &request.selection {
            let saved = crate::runs::Saved::read(&selection.run)?;
            saved.manifest.verify(&saved.path)?;
            self.bind_saved(request, saved)?;
        }
        Ok(())
    }

    fn bind_saved(&mut self, request: &mut Request, saved: crate::runs::Saved) -> Result<()> {
        if request.config.workload.package != saved.manifest.config.workload.package {
            return Err("prepared workload package differs from its saved recording".into());
        }
        if request.operation == Operation::Search && saved.manifest.mode != "branch" {
            return Err(
                "search --from requires a branch; use branch NAME to choose a starting point"
                    .into(),
            );
        }
        request.config = saved.manifest.config.clone();
        validate_request_config(request)?;
        self.saved = Some(saved);
        Ok(())
    }
}

#[cfg(any(feature = "frontend", test))]
struct RecordWriter {
    inner: fs::File,
    digest: Sha256,
    bytes: u64,
}

#[cfg(any(feature = "frontend", test))]
impl Write for RecordWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if (bytes.len() as u64) > (LIMIT - 32).saturating_sub(self.bytes) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "prepared execution exceeds its byte limit",
            ));
        }
        let count = self.inner.write(bytes)?;
        self.digest.update(&bytes[..count]);
        self.bytes += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn validate_request_inputs(request: &Request) -> Result<()> {
    let saved = request.selection.is_some();
    let faults = request.config.workload.package == "faults";
    let nes = request.config.workload.package == "nes";
    let branch = request.operation == Operation::Branch;
    if (request.operation == Operation::Run && saved)
        || (matches!(
            request.operation,
            Operation::Replay | Operation::Branch | Operation::Resume | Operation::Shrink
        ) && !saved)
        || (request.operation == Operation::Resume
            && request
                .selection
                .as_ref()
                .is_some_and(|selection| selection.finding.is_some()))
    {
        return Err("prepared selection differs from its operation".into());
    }
    if request.actions.is_some()
        && (!(faults || nes) || !matches!(request.operation, Operation::Run | Operation::Branch))
    {
        return Err("prepared operation does not accept captured actions".into());
    }
    if !request.command.is_empty() && !(faults && request.operation == Operation::Run) {
        return Err("prepared operation does not accept command overrides".into());
    }
    let commands = usize::from(request.exec.is_some())
        + usize::from(request.exec_file.is_some())
        + usize::from(request.shell);
    if commands > 1 || (commands != 0 && !(faults && branch)) {
        return Err("prepared operation does not accept these guest commands".into());
    }
    if request.stop && !(branch && (faults || nes)) {
        return Err("prepared operation does not accept stopping the suffix".into());
    }
    if request.console && !(faults && request.operation == Operation::Run) {
        return Err("prepared operation does not accept a live console".into());
    }
    let points = usize::from(request.point.step.is_some())
        + usize::from(request.point.rewind.is_some())
        + usize::from(request.point.rewind_time.is_some());
    if points > 1
        || (points != 0
            && !((faults || nes) && (branch || (saved && request.operation == Operation::Search))))
    {
        return Err("prepared operation does not accept these point selectors".into());
    }
    if request.repeat == 0
        || (request.repeat != 1
            && !matches!(request.operation, Operation::Run | Operation::Replay)
            && !(nes && branch))
    {
        return Err("prepared operation does not accept these repeats".into());
    }
    if request.operation == Operation::Shrink && !faults {
        return Err("prepared shrink requires a fault recording".into());
    }
    if request.jobs == 0 || (request.jobs != 1 && request.operation != Operation::Shrink) {
        return Err("prepared operation does not accept parallel replays".into());
    }
    let budget = &request.budget;
    if (budget.seed.is_some() || budget.executions.is_some() || budget.wall_seconds.is_some())
        && !((faults || nes)
            && saved
            && matches!(request.operation, Operation::Search | Operation::Resume))
    {
        return Err("prepared operation does not accept additional search budgets".into());
    }
    Ok(())
}

fn validate_request_config(request: &Request) -> Result<()> {
    request.config.validate()?;
    if request.config.runner.kind != "consonance"
        || !matches!(
            request.operation,
            Operation::Search
                | Operation::Run
                | Operation::Replay
                | Operation::Branch
                | Operation::Resume
                | Operation::Shrink
        )
    {
        return Err("the guarded runtime accepts only prepared Consonance execution".into());
    }
    validate_request_inputs(request)?;
    let runtime = crate::runners::consonance(&request.config.runner)?;
    if runtime.backend == Backend::Auto {
        return Err("prepared execution must pin its backend".into());
    }
    match request.config.workload.package.as_str() {
        "faults" => {
            let config = crate::adapters::faults::config::Config::from_shared(&request.config)?;
            if config.build.language.is_some()
                || config.build.dockerfile.is_some()
                || !config.build.command.is_empty()
            {
                return Err("prepared execution still contains build work".into());
            }
            if !request.command.is_empty() && request.command != config.command {
                return Err(
                    "prepared command override differs from its effective configuration".into(),
                );
            }
        }
        "nes" => {
            if runtime.backend != Backend::Kvm {
                return Err("the NES guest adapter requires KVM".into());
            }
        }
        "nested" => {
            if !request.config.workload.options.is_empty() {
                return Err("nested has no workload options".into());
            }
            if runtime.backend != Backend::Kvm
                || !matches!(request.operation, Operation::Search | Operation::Replay)
                || (request.selection.is_some() && request.operation != Operation::Replay)
            {
                return Err("nested requires KVM search or replay".into());
            }
        }
        _ => return Err("unknown prepared workload package".into()),
    }
    Ok(())
}

impl Record {
    #[cfg(any(feature = "frontend", test))]
    pub fn new(request: &Request) -> Result<Self> {
        let request = serde_json::to_vec(request)?;
        if request.len() > REQUEST_LIMIT {
            return Err("prepared request exceeds its metadata limit".into());
        }
        Ok(Self {
            format: FORMAT.into(),
            release: crate::runtime::RELEASE.into(),
            source: env!("HARMONY_PLATFORM_SOURCE_DIGEST").into(),
            request,
            kernel: Vec::new(),
            base: Vec::new(),
            runtime_manifest: None,
            execution: None,
            rom: None,
            inputs: BTreeMap::new(),
            profile: Vec::new(),
        })
    }

    #[cfg(any(feature = "frontend", test))]
    pub fn write(&self, path: &Path) -> Result<()> {
        let mut writer = BufWriter::with_capacity(
            1 << 20,
            RecordWriter {
                inner: fs::File::create(path)?,
                digest: Sha256::new(),
                bytes: 0,
            },
        );
        postcard::to_io(self, &mut writer)?;
        let mut record = writer.into_inner().map_err(|error| error.into_error())?;
        let checksum = record.digest.clone().finalize();
        record.inner.write_all(&checksum)?;
        record.inner.flush()?;
        Ok(())
    }

    pub fn read(path: &Path) -> Result<(Request, Prepared)> {
        let file = fs::File::open(path)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() > LIMIT {
            return Err("prepared execution is not a bounded regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT {
            return Err("prepared execution exceeds its byte limit".into());
        }
        let payload = bytes
            .len()
            .checked_sub(32)
            .ok_or("prepared execution checksum is missing")?;
        if bytes[payload..] != Sha256::digest(&bytes[..payload])[..] {
            return Err("prepared execution checksum differs from its bytes".into());
        }
        let (record, remaining): (Self, _) = postcard::take_from_bytes(&bytes[..payload])?;
        if !remaining.is_empty() {
            return Err("prepared execution contains trailing record bytes".into());
        }
        record.accept()
    }

    fn accept(self) -> Result<(Request, Prepared)> {
        if self.format != FORMAT
            || self.release != crate::runtime::RELEASE
            || self.source != env!("HARMONY_PLATFORM_SOURCE_DIGEST")
        {
            return Err(
                "prepared execution requires a matching Harmony release and protocol".into(),
            );
        }
        if self.request.len() > REQUEST_LIMIT || self.profile.len() > 4096 || self.inputs.len() > 2
        {
            return Err("prepared execution metadata exceeds its limit".into());
        }
        let mut request: Request = serde_json::from_slice(&self.request)?;
        validate_request_config(&request)?;
        if self.rom.is_some() && request.config.workload.package != "nes" {
            return Err("unexpected prepared ROM for a non-NES workload".into());
        }
        for (name, field) in [
            ("actions", &request.actions),
            ("exec-file", &request.exec_file),
        ] {
            if self.inputs.contains_key(name) != field.is_some() {
                return Err("prepared captured input presence differs from its request".into());
            }
        }
        let runtime = crate::runners::consonance(&request.config.runner)?;
        let files = tempfile::tempdir()?;
        for (name, bytes) in self.inputs {
            let field = match name.as_str() {
                "actions" => &mut request.actions,
                "exec-file" => &mut request.exec_file,
                _ => return Err("unknown prepared input".into()),
            };
            if field.is_none() {
                return Err("unexpected prepared input".into());
            }
            let path = files.path().join(name);
            fs::write(&path, bytes)?;
            *field = Some(path);
        }
        for field in [&request.actions, &request.exec_file] {
            if field
                .as_ref()
                .is_some_and(|path| !path.starts_with(files.path()))
            {
                return Err("prepared execution omitted a captured input".into());
            }
        }
        if request.selection.is_none() {
            crate::runtime::validate_captured_runtime(
                &runtime,
                &self.kernel,
                &self.base,
                self.runtime_manifest.as_deref(),
            )?;
            if self.kernel.is_empty() || self.base.is_empty() || self.execution.is_none() {
                return Err("prepared execution has no runtime or application".into());
            }
        } else if !self.kernel.is_empty()
            || !self.base.is_empty()
            || self.execution.is_some()
            || self.runtime_manifest.is_some()
            || self.rom.is_some()
        {
            return Err("saved execution cannot replace its recorded artifacts".into());
        }
        if runtime.backend == Backend::Uml && request.selection.is_none() {
            let profile = files.path().join("uml");
            fs::create_dir(&profile)?;
            let mut paths = std::collections::BTreeSet::new();
            for item in self.profile {
                if item.path.as_os_str().is_empty()
                    || item
                        .path
                        .components()
                        .any(|part| !matches!(part, std::path::Component::Normal(_)))
                    || item.mode & !0o777 != 0
                    || !paths.insert(item.path.clone())
                {
                    return Err("invalid prepared UML profile file".into());
                }
                let path = profile.join(item.path);
                fs::create_dir_all(path.parent().ok_or("profile file has no parent")?)?;
                fs::write(&path, item.bytes)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(path, fs::Permissions::from_mode(item.mode))?;
                }
            }
            let admitted = uml::Profile::load(&profile)?;
            let kernel = fs::read(
                admitted
                    .directory()
                    .join(&admitted.profile().executable.name),
            )?;
            if kernel != self.kernel {
                return Err("prepared UML profile differs from the kernel".into());
            }
            let mut runtime = runtime.clone();
            runtime.uml_profile = Some(profile);
            runtime.kernel = Some(
                admitted
                    .directory()
                    .join(&admitted.profile().executable.name),
            );
            crate::runners::store_consonance(&mut request.config.runner, &runtime)?;
        } else if !self.profile.is_empty() {
            return Err("unexpected prepared UML profile".into());
        }
        let execution = self
            .execution
            .map(oci_support::PreparedExecution::from_record)
            .transpose()?;
        let mut bundle = None;
        let mut vocabulary = None;
        match request.config.workload.package.as_str() {
            "faults" => {
                let config = crate::adapters::faults::config::Config::from_shared(&request.config)?;
                if let Some(execution) = &execution {
                    let mode = if config.backend == Backend::Uml {
                        faults_workload::admission::ExecutionMode::Uml
                    } else {
                        faults_workload::admission::ExecutionMode::Vm
                    };
                    if execution.exposes_kvm() {
                        return Err("fault execution cannot expose host KVM".into());
                    }
                    let supervised = request.operation == Operation::Search
                        || request.actions.is_some()
                        || !config.nodes.is_empty();
                    if execution.execution().bundle.is_some() != supervised {
                        return Err("prepared fault supervision differs from its request".into());
                    }
                    if supervised && request.console {
                        return Err(
                            "prepared supervised execution does not accept a live console".into(),
                        );
                    }
                    if request.operation == Operation::Run
                        && !config.command.is_empty()
                        && (request.actions.is_some() || !config.nodes.is_empty())
                    {
                        return Err("command overrides cannot be combined with supervised scenarios; set nodes.NAME.command in TOML".into());
                    }
                    if supervised {
                        let generated = config.bundle()?;
                        let (text, admitted) = faults_workload::prepare::admit_prepared(
                            execution,
                            generated.as_deref(),
                            mode,
                        )?;
                        bundle = Some(text);
                        vocabulary = Some(admitted);
                    } else {
                        if !config.command.is_empty()
                            && execution.execution().argv != config.command
                        {
                            return Err(
                                "prepared command differs from the requested command".into()
                            );
                        }
                        if config.bundle()?.is_some() || !execution.external_inputs().is_empty() {
                            return Err(
                                "prepared command cannot replace a supervisor scenario".into()
                            );
                        }
                        let archive = execution.archived_rootfs()?;
                        faults_workload::admission::ArchivedImage::new(&archive)
                            .inspect()?
                            .require_for(mode)?;
                    }
                }
            }
            "nes" => {
                if let Some(execution) = &execution {
                    let rom = self.rom.as_ref().ok_or("prepared NES input is missing")?;
                    nes_workload::package::RomKind::identify(rom)?;
                    let inputs = execution.external_inputs();
                    if execution.exposes_kvm()
                        || inputs.len() != 1
                        || inputs[0].destination != Path::new(nes_workload::prepare::ROM_PATH)
                        || inputs[0].data != *rom
                        || execution.execution().argv
                            != [
                                nes_workload::prepare::AGENT_PATH,
                                "--nes-payload",
                                "--rom",
                                nes_workload::prepare::ROM_PATH,
                            ]
                        || execution.execution().bundle.is_some()
                    {
                        return Err("prepared NES execution differs from its typed input".into());
                    }
                }
            }
            "nested" => {
                if let Some(execution) = &execution {
                    let inputs = execution.external_inputs();
                    if !execution.exposes_kvm()
                        || !inputs.is_empty()
                        || execution.execution().bundle.is_some()
                        || execution.execution().argv != ["/app/nested-driver", "--sdk", "--search"]
                    {
                        return Err("prepared nested execution differs from its contract".into());
                    }
                }
            }
            _ => return Err("unknown prepared workload package".into()),
        }
        Ok((
            request,
            Prepared {
                kernel: self.kernel,
                base: self.base,
                execution,
                rom: self.rom,
                bundle,
                vocabulary,
                saved: None,
                _files: files,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Record {
        let root = tempfile::tempdir().unwrap();
        let image = oci_support::image::StagedImage {
            rootfs: root.path().to_owned(),
            owners: Default::default(),
            config: Default::default(),
        };
        let launch = oci_support::LaunchRequest::new(vec![
            "/app/nested-driver".into(),
            "--sdk".into(),
            "--search".into(),
        ])
        .with_kvm();
        let execution = oci_support::bundle::prepare(&image, &launch).unwrap();
        let mut request = Request {
            operation: Operation::Search,
            ..Default::default()
        };
        request.config.workload.package = "nested".into();
        request.config.runner.kind = "consonance".into();
        request.config.runner.backend = Some("kvm".into());
        let mut record = Record::new(&request).unwrap();
        record.kernel = b"captured kernel".to_vec();
        record.base = b"captured base".to_vec();
        record.execution = Some(execution.record());
        record
    }

    fn fault_record(operation: Operation, actions: bool, nodes: bool, supervised: bool) -> Record {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("etc/harmony")).unwrap();
        fs::write(
            root.path().join("etc/harmony/bundle"),
            b"node app /app/service\n",
        )
        .unwrap();
        let image = oci_support::image::StagedImage {
            rootfs: root.path().to_owned(),
            owners: Default::default(),
            config: oci_support::image::RuntimeConfig {
                cmd: vec!["/app/service".into()],
                ..Default::default()
            },
        };
        let mut config = crate::adapters::faults::config::Config {
            backend: Backend::Kvm,
            ..Default::default()
        };
        if nodes {
            config.nodes.insert(
                "app".into(),
                crate::adapters::faults::config::Node {
                    command: vec!["/app/service".into()],
                },
            );
        }
        let launch = if supervised {
            if let Some(bundle) = config.bundle().unwrap() {
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
            oci_support::LaunchRequest::default()
        };
        let execution = oci_support::bundle::prepare(&image, &launch).unwrap();
        let request = Request {
            operation,
            config: config.shared().unwrap(),
            actions: actions.then(|| "captured-actions".into()),
            ..Default::default()
        };
        let mut record = Record::new(&request).unwrap();
        record.kernel = b"captured kernel".to_vec();
        record.base = b"captured base".to_vec();
        record.execution = Some(execution.record());
        if actions {
            record.inputs.insert("actions".into(), b"[]".to_vec());
        }
        record
    }

    #[test]
    fn handoff_roundtrips_plain_and_required_supervised_fault_requests() {
        for (operation, actions, nodes, supervised) in [
            (Operation::Run, false, false, false),
            (Operation::Search, false, false, true),
            (Operation::Run, true, false, true),
            (Operation::Run, false, true, true),
        ] {
            let record = fault_record(operation, actions, nodes, supervised);
            let file = tempfile::NamedTempFile::new().unwrap();
            record.write(file.path()).unwrap();
            let (request, prepared) = Record::read(file.path()).unwrap();
            assert_eq!(request.operation, operation);
            assert_eq!(prepared.bundle.is_some(), supervised);
            assert_eq!(prepared.vocabulary.is_some(), supervised);
            if actions {
                assert_eq!(fs::read(request.actions.unwrap()).unwrap(), b"[]");
            }
            assert_eq!(prepared.kernel, b"captured kernel");
            assert_eq!(prepared.base, b"captured base");
        }
    }

    #[test]
    fn handoff_rejects_rechecksummed_unused_execution_inputs() {
        for case in 0..15 {
            let mut record = fault_record(Operation::Search, false, false, true);
            let mut request: Request = serde_json::from_slice(&record.request).unwrap();
            match case {
                0 => {
                    request.actions = Some("actions".into());
                    record.inputs.insert("actions".into(), b"[]".to_vec());
                }
                1 => request.command = vec!["/bin/false".into()],
                2 => request.exec = Some("false".into()),
                3 => {
                    request.exec_file = Some("script".into());
                    record.inputs.insert("exec-file".into(), b"false".to_vec());
                }
                4 => request.shell = true,
                5 => request.stop = true,
                6 => request.console = true,
                7 => request.point.step = Some(0),
                8 => request.point.rewind = Some(1),
                9 => request.point.rewind_time = Some("1s".into()),
                10 => request.repeat = 2,
                11 => request.repeat = 0,
                12 => request.budget.executions = Some(1),
                13 => request.budget.seed = Some(1),
                _ => request.budget.wall_seconds = Some(1),
            }
            record.request = serde_json::to_vec(&request).unwrap();
            let file = tempfile::NamedTempFile::new().unwrap();
            record.write(file.path()).unwrap();
            let error = Record::read(file.path()).err().unwrap().to_string();
            assert!(error.contains("prepared operation"), "case {case}: {error}");
        }
    }

    #[test]
    fn handoff_rejects_rechecksummed_unused_roms_and_mismatched_capture_presence() {
        for package in ["faults", "nested"] {
            let mut record = if package == "faults" {
                fault_record(Operation::Search, false, false, true)
            } else {
                record()
            };
            record.rom = Some(b"unused ROM".to_vec());
            let file = tempfile::NamedTempFile::new().unwrap();
            record.write(file.path()).unwrap();
            assert!(
                Record::read(file.path())
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("unexpected prepared ROM")
            );
        }
        for extra in [false, true] {
            let mut record = fault_record(Operation::Run, !extra, false, !extra);
            if extra {
                record.inputs.insert("exec-file".into(), b"true".to_vec());
            } else {
                record.inputs.clear();
            }
            let file = tempfile::NamedTempFile::new().unwrap();
            record.write(file.path()).unwrap();
            assert!(
                Record::read(file.path())
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("captured input presence differs")
            );
        }
    }

    #[test]
    fn handoff_keeps_fresh_nes_rom_identification_after_payload_role_checks() {
        let root = tempfile::tempdir().unwrap();
        let image = oci_support::image::StagedImage {
            rootfs: root.path().to_owned(),
            owners: Default::default(),
            config: Default::default(),
        };
        let rom = b"unsupported ROM";
        let execution = nes_workload::prepare::prepare_oci(&image, rom).unwrap();
        let mut request = Request {
            operation: Operation::Search,
            ..Default::default()
        };
        request.config.workload.package = "nes".into();
        request.config.runner.kind = "consonance".into();
        request.config.runner.backend = Some("kvm".into());
        let mut record = Record::new(&request).unwrap();
        record.kernel = b"captured kernel".to_vec();
        record.base = b"captured base".to_vec();
        record.execution = Some(execution.record());
        record.rom = Some(rom.to_vec());
        let file = tempfile::NamedTempFile::new().unwrap();
        record.write(file.path()).unwrap();
        assert!(
            Record::read(file.path())
                .err()
                .unwrap()
                .to_string()
                .contains("unsupported NES ROM SHA-256")
        );
    }

    #[test]
    fn handoff_checks_raw_commands_and_supervised_console_before_execution() {
        for case in 0..3 {
            let mut record = fault_record(Operation::Run, false, case != 0, case != 0);
            let mut request: Request = serde_json::from_slice(&record.request).unwrap();
            if case < 2 {
                request.command = vec!["/bin/false".into()];
            } else {
                request.console = true;
            }
            record.request = serde_json::to_vec(&request).unwrap();
            let file = tempfile::NamedTempFile::new().unwrap();
            record.write(file.path()).unwrap();
            let error = Record::read(file.path()).err().unwrap().to_string();
            assert!(
                error.contains(if case < 2 {
                    "command override differs"
                } else {
                    "supervised execution does not accept a live console"
                }),
                "{error}"
            );
        }
        let mut record = fault_record(Operation::Run, false, false, false);
        let mut request: Request = serde_json::from_slice(&record.request).unwrap();
        let mut config =
            crate::adapters::faults::config::Config::from_shared(&request.config).unwrap();
        config.command = vec!["/app/service".into()];
        request.command = config.command.clone();
        request.config = config.shared().unwrap();
        request.console = true;
        record.request = serde_json::to_vec(&request).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        record.write(file.path()).unwrap();
        let (request, prepared) = Record::read(file.path()).unwrap();
        assert_eq!(request.command, ["/app/service"]);
        assert!(request.console && prepared.bundle.is_none());
    }

    fn saved_record(operation: Operation, package: &str) -> Record {
        let mut request = Request {
            operation,
            selection: Some(crate::selection::Selection {
                run: "saved-recording".into(),
                finding: None,
            }),
            ..Default::default()
        };
        request.config.workload.package = package.into();
        request.config.runner.kind = "consonance".into();
        request.config.runner.backend = Some("kvm".into());
        Record::new(&request).unwrap()
    }

    #[test]
    fn handoff_preserves_saved_execution_inputs_for_their_consumers() {
        for (operation, package) in [
            (Operation::Branch, "faults"),
            (Operation::Branch, "nes"),
            (Operation::Replay, "faults"),
            (Operation::Replay, "nes"),
            (Operation::Replay, "nested"),
            (Operation::Search, "faults"),
            (Operation::Search, "nes"),
            (Operation::Resume, "faults"),
            (Operation::Resume, "nes"),
            (Operation::Shrink, "faults"),
        ] {
            let mut record = saved_record(operation, package);
            let mut request: Request = serde_json::from_slice(&record.request).unwrap();
            match operation {
                Operation::Branch => {
                    request.actions = Some("actions".into());
                    record.inputs.insert("actions".into(), b"[]".to_vec());
                    request.point.step = Some(0);
                    request.stop = true;
                    if package == "faults" {
                        request.exec_file = Some("script".into());
                        record.inputs.insert("exec-file".into(), b"true".to_vec());
                    } else {
                        request.repeat = 2;
                    }
                }
                Operation::Replay => request.repeat = 2,
                Operation::Search => {
                    request.point.step = Some(0);
                    request.budget.executions = Some(1);
                }
                Operation::Resume => request.budget.wall_seconds = Some(1),
                Operation::Shrink => request.jobs = 4,
                _ => unreachable!(),
            }
            record.request = serde_json::to_vec(&request).unwrap();
            let file = tempfile::NamedTempFile::new().unwrap();
            record.write(file.path()).unwrap();
            let (request, prepared) = Record::read(file.path()).unwrap();
            assert_eq!(request.operation, operation);
            assert_eq!(
                request.jobs,
                if operation == Operation::Shrink { 4 } else { 1 }
            );
            assert!(request.selection.is_some() && prepared.execution.is_none());
            if let Some(path) = request.actions {
                assert_eq!(fs::read(path).unwrap(), b"[]");
            }
            if let Some(path) = request.exec_file {
                assert_eq!(fs::read(path).unwrap(), b"true");
            }
        }
    }

    #[test]
    fn only_a_fault_shrink_accepts_parallel_replays() {
        for (operation, package, jobs, refusal) in [
            (Operation::Replay, "faults", 4, "parallel replays"),
            (Operation::Shrink, "faults", 0, "parallel replays"),
            (Operation::Shrink, "nes", 1, "fault recording"),
        ] {
            let mut record = saved_record(operation, package);
            let mut request: Request = serde_json::from_slice(&record.request).unwrap();
            request.jobs = jobs;
            record.request = serde_json::to_vec(&request).unwrap();
            let file = tempfile::NamedTempFile::new().unwrap();
            record.write(file.path()).unwrap();
            let Err(error) = Record::read(file.path()) else {
                panic!("{operation:?} {package} {jobs}");
            };
            assert!(error.to_string().contains(refusal), "{error}");
        }
    }

    fn write_saved_metadata(path: &Path, config: &crate::config::Config, mode: &str) {
        fs::write(
            path.join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "format": "harmony-run-v4",
                "binary_identity": "metadata admission fixture, not a strict image",
                "architecture": std::env::consts::ARCH,
                "mode": mode,
                "status": "complete",
                "error": null,
                "config": config,
                "parent": null,
                "artifacts": {},
                "payload": null,
                "runner_identity": null,
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn read_saved_handoff(mut record: Record, directory: &Path) -> (Request, Prepared) {
        let mut request: Request = serde_json::from_slice(&record.request).unwrap();
        request.selection.as_mut().unwrap().run = directory.display().to_string();
        record.request = serde_json::to_vec(&request).unwrap();
        let file = tempfile::NamedTempFile::new().unwrap();
        record.write(file.path()).unwrap();
        Record::read(file.path()).unwrap()
    }

    #[test]
    fn saved_metadata_admission_rejects_package_repeat_and_search_source_substitution() {
        let directory = tempfile::tempdir().unwrap();
        let mut record = saved_record(Operation::Branch, "nes");
        let mut request: Request = serde_json::from_slice(&record.request).unwrap();
        request.repeat = 2;
        request.point.step = Some(0);
        record.request = serde_json::to_vec(&request).unwrap();
        let (mut request, mut prepared) = read_saved_handoff(record, directory.path());
        let mut original = request.config.clone();
        original.workload.package = "faults".into();
        write_saved_metadata(directory.path(), &original, "search");
        let saved = crate::runs::Saved::read(&request.selection.as_ref().unwrap().run).unwrap();
        let error = prepared.bind_saved(&mut request, saved).unwrap_err();
        assert!(error.to_string().contains("workload package differs"));
        assert!(prepared.saved.is_none());

        let (mut request, mut prepared) =
            read_saved_handoff(saved_record(Operation::Search, "faults"), directory.path());
        write_saved_metadata(directory.path(), &request.config, "search");
        let saved = crate::runs::Saved::read(&request.selection.as_ref().unwrap().run).unwrap();
        let error = prepared.bind_saved(&mut request, saved).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("search --from requires a branch")
        );
        assert!(prepared.saved.is_none());
    }

    #[test]
    fn saved_metadata_admission_validates_the_effective_config_before_holding_it() {
        for case in 0..4 {
            let directory = tempfile::tempdir().unwrap();
            let package = if case == 2 { "nes" } else { "faults" };
            let (mut request, mut prepared) =
                read_saved_handoff(saved_record(Operation::Replay, package), directory.path());
            let mut original = request.config.clone();
            let expected = match case {
                0 => {
                    original.search.executions = Some(u64::MAX);
                    "exceeds TOML's maximum"
                }
                1 => {
                    original.workload.options.insert(
                        "build".into(),
                        toml::Value::Table(
                            [(
                                "command".into(),
                                toml::Value::Array(vec![toml::Value::String("make".into())]),
                            )]
                            .into_iter()
                            .collect(),
                        ),
                    );
                    "still contains build work"
                }
                2 => {
                    original.runner.backend = Some("hvf".into());
                    "NES guest adapter requires KVM"
                }
                _ => {
                    original.runner.kind = "quicknes".into();
                    "only prepared Consonance execution"
                }
            };
            write_saved_metadata(directory.path(), &original, "run");
            let saved = crate::runs::Saved::read(&request.selection.as_ref().unwrap().run).unwrap();
            let error = prepared.bind_saved(&mut request, saved).unwrap_err();
            assert!(error.to_string().contains(expected), "case {case}: {error}");
            assert!(prepared.saved.is_none());
        }
    }

    #[test]
    fn saved_metadata_admission_keeps_one_config_when_manifest_is_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let (mut request, mut prepared) =
            read_saved_handoff(saved_record(Operation::Search, "faults"), directory.path());
        let mut original = request.config.clone();
        original.search.seed = 17;
        write_saved_metadata(directory.path(), &original, "branch");
        let saved = crate::runs::Saved::read(&request.selection.as_ref().unwrap().run).unwrap();
        prepared.bind_saved(&mut request, saved).unwrap();
        assert_eq!(request.config.search.seed, 17);
        let mut replacement = original;
        replacement.workload.package = "nes".into();
        write_saved_metadata(directory.path(), &replacement, "search");
        let held = prepared.saved.take().unwrap();
        assert_eq!(held.path, directory.path().canonicalize().unwrap());
        assert_eq!(held.manifest.config.workload.package, "faults");
        assert_eq!(held.manifest.config.search.seed, request.config.search.seed);
        assert_eq!(held.manifest.mode, "branch");
        assert_eq!(
            crate::runs::Manifest::read(directory.path())
                .unwrap()
                .config
                .workload
                .package,
            "nes"
        );
    }

    #[test]
    fn handoff_rejects_unused_saved_inputs_and_conflicting_branch_commands() {
        for case in 0..8 {
            let mut record = saved_record(Operation::Replay, "faults");
            let mut request: Request = serde_json::from_slice(&record.request).unwrap();
            match case {
                0 => {
                    request.actions = Some("actions".into());
                    record.inputs.insert("actions".into(), b"[]".to_vec());
                }
                1 => {
                    request.exec_file = Some("script".into());
                    record.inputs.insert("exec-file".into(), b"true".to_vec());
                }
                2 => request.point.step = Some(0),
                3 => {
                    request.operation = Operation::Branch;
                    request.exec = Some("true".into());
                    request.shell = true;
                }
                4 => {
                    request.operation = Operation::Branch;
                    request.point.step = Some(0);
                    request.point.rewind = Some(1);
                }
                5 => {
                    request.operation = Operation::Resume;
                    request.selection.as_mut().unwrap().finding = Some(1);
                }
                6 => request.selection = None,
                _ => request.operation = Operation::Run,
            }
            record.request = serde_json::to_vec(&request).unwrap();
            let file = tempfile::NamedTempFile::new().unwrap();
            record.write(file.path()).unwrap();
            assert!(Record::read(file.path()).is_err(), "case {case}");
        }
    }

    #[test]
    fn handoff_rejects_rechecksummed_fault_supervision_mismatches() {
        for (operation, actions, nodes, supervised) in [
            (Operation::Search, false, false, false),
            (Operation::Run, true, false, false),
            (Operation::Run, false, true, false),
            (Operation::Run, false, false, true),
        ] {
            let record = fault_record(operation, actions, nodes, supervised);
            let file = tempfile::NamedTempFile::new().unwrap();
            record.write(file.path()).unwrap();
            assert!(
                Record::read(file.path())
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("supervision differs")
            );
        }
    }

    #[test]
    fn handoff_rejects_rechecksummed_bytes_after_a_valid_record() {
        let record = record();
        let file = tempfile::NamedTempFile::new().unwrap();
        record.write(file.path()).unwrap();
        assert!(Record::read(file.path()).is_ok());
        let bytes = fs::read(file.path()).unwrap();
        let payload = &bytes[..bytes.len() - 32];
        for suffix in [vec![0], postcard::to_allocvec(&record).unwrap()] {
            let mut changed = payload.to_vec();
            changed.extend(suffix);
            let checksum = Sha256::digest(&changed);
            changed.extend_from_slice(&checksum);
            fs::write(file.path(), changed).unwrap();
            assert!(
                Record::read(file.path())
                    .err()
                    .unwrap()
                    .to_string()
                    .contains("trailing record bytes")
            );
        }
    }

    #[test]
    fn handoff_uses_captured_artifacts_and_inputs_after_sources_change() {
        let mut record = fault_record(Operation::Run, true, false, true);
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("actions");
        fs::write(&original, b"original actions").unwrap();
        let mut request: Request = serde_json::from_slice(&record.request).unwrap();
        request.actions = Some(original.clone());
        record.request = serde_json::to_vec(&request).unwrap();
        record
            .inputs
            .insert("actions".into(), fs::read(&original).unwrap());
        let path = directory.path().join("prepared");
        record.write(&path).unwrap();
        fs::write(original, b"replacement actions").unwrap();
        let (request, prepared) = Record::read(&path).unwrap();
        assert_eq!(
            fs::read(request.actions.unwrap()).unwrap(),
            b"original actions"
        );
        assert_eq!(prepared.kernel, b"captured kernel");
        assert_eq!(prepared.base, b"captured base");
        assert!(prepared.bundle.is_some());
    }

    #[test]
    fn handoff_refuses_role_version_and_uncaptured_input_changes() {
        for change in 0..6 {
            let mut record = if change == 5 {
                let mut record = fault_record(Operation::Run, true, false, true);
                record.inputs.clear();
                record
            } else {
                record()
            };
            let mut request: Request = serde_json::from_slice(&record.request).unwrap();
            match change {
                0 => record.format.push('x'),
                1 => record.source.push('x'),
                2 => record.release.push('x'),
                3 => request.operation = Operation::Prepare,
                4 => request.config.runner.kind = "quicknes".into(),
                _ => request.actions = Some("uncaptured actions".into()),
            }
            record.request = serde_json::to_vec(&request).unwrap();
            let file = tempfile::NamedTempFile::new().unwrap();
            record.write(file.path()).unwrap();
            assert!(Record::read(file.path()).is_err());
        }
    }

    #[test]
    fn handoff_rejects_changed_bytes_before_decoding_the_request() {
        let record = record();
        let file = tempfile::NamedTempFile::new().unwrap();
        record.write(file.path()).unwrap();
        let mut bytes = fs::read(file.path()).unwrap();
        bytes[16] ^= 1;
        fs::write(file.path(), bytes).unwrap();
        assert!(
            Record::read(file.path())
                .err()
                .unwrap()
                .to_string()
                .contains("checksum differs")
        );
    }

    #[test]
    fn handoff_rejects_recommitted_execution_and_oversized_files() {
        let mut record = record();
        let root = tempfile::tempdir().unwrap();
        let image = oci_support::image::StagedImage {
            rootfs: root.path().to_owned(),
            owners: Default::default(),
            config: Default::default(),
        };
        let launch = oci_support::LaunchRequest::new(vec![
            "/app/nested-driver".into(),
            "--sdk".into(),
            "--search".into(),
        ]);
        record.execution = Some(
            oci_support::bundle::prepare(&image, &launch)
                .unwrap()
                .record(),
        );
        assert!(
            record
                .accept()
                .err()
                .unwrap()
                .to_string()
                .contains("nested execution differs")
        );
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(LIMIT + 1).unwrap();
        assert!(
            Record::read(file.path())
                .err()
                .unwrap()
                .to_string()
                .contains("bounded")
        );
    }
}
