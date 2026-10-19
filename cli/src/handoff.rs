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
    _files: tempfile::TempDir,
}

#[cfg(any(feature = "frontend", test))]
struct RecordWriter {
    inner: BufWriter<fs::File>,
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
        let mut writer = RecordWriter {
            inner: BufWriter::new(fs::File::create(path)?),
            digest: Sha256::new(),
            bytes: 0,
        };
        postcard::to_io(self, &mut writer)?;
        let checksum = writer.digest.clone().finalize();
        writer.inner.write_all(&checksum)?;
        writer.inner.flush()?;
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
        request.config.validate()?;
        if request.config.runner.kind != "consonance"
            || !matches!(
                request.operation,
                Operation::Search
                    | Operation::Run
                    | Operation::Replay
                    | Operation::Branch
                    | Operation::Resume
            )
        {
            return Err("the guarded runtime accepts only prepared Consonance execution".into());
        }
        let runtime = crate::runners::consonance(&request.config.runner)?;
        if runtime.backend == Backend::Auto {
            return Err("prepared execution must pin its backend".into());
        }
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
                if config.build.language.is_some()
                    || config.build.dockerfile.is_some()
                    || !config.build.command.is_empty()
                {
                    return Err("prepared execution still contains build work".into());
                }
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
                if runtime.backend != Backend::Kvm {
                    return Err("the NES guest adapter requires KVM".into());
                }
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
                if !request.config.workload.options.is_empty() {
                    return Err("nested has no workload options".into());
                }
                if runtime.backend != Backend::Kvm
                    || !matches!(request.operation, Operation::Search | Operation::Replay)
                {
                    return Err("nested requires KVM search or replay".into());
                }
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
        let mut record = record();
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
        assert!(prepared.execution.unwrap().exposes_kvm());
    }

    #[test]
    fn handoff_refuses_role_version_and_uncaptured_input_changes() {
        for change in 0..6 {
            let mut record = record();
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
